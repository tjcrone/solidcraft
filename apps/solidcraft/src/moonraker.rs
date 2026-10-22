//! The printer host for a Moonraker printer (Klipper): uploads to `/server/files/upload`, the
//! print state and temperatures from `/printer/objects/query`, cancel through
//! `/printer/print/cancel`. Every request runs on its own thread with a timeout, so the engine
//! and the window never wait on the printer; a status thread polls every two seconds and keeps
//! the last answer, with how old it is, for a printer that goes quiet (a Wi-Fi dongle asleep,
//! a reboot) and comes back. Nothing is ever sent twice on its own: an upload that gets no answer
//! is reported as such, and the user decides.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use solidcraft_engine::printer_host::{Heater, JobState, PrinterHost, PrinterInfo, PrinterStatus};

const POLL_EVERY: Duration = Duration::from_secs(2);
const POLL_TIMEOUT: Duration = Duration::from_secs(4);
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(180);
const CANCEL_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Most bytes of an answer that are read.
const ANSWER_LIMIT: u64 = 1 << 20;

pub struct Moonraker {
    shared: Arc<Shared>,
}

struct Shared {
    url: String,
    jobs: Mutex<HashMap<u64, JobState>>,
    next_job: AtomicU64,
    snapshot: Mutex<Snapshot>,
}

#[derive(Default)]
struct Snapshot {
    status: PrinterStatus,
    last_ok: Option<Instant>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// An HTTP agent for one kind of request: plain HTTP to the printer on the LAN, or HTTPS with
/// the platform's roots; no redirects (an upload must not wander); answers limited by the callers.
fn agent(timeout: Duration) -> ureq::Agent {
    let tls = ureq::tls::TlsConfig::builder()
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .unversioned_rustls_crypto_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .build();
    ureq::Agent::config_builder()
        .tls_config(tls)
        .http_status_as_error(false)
        .timeout_global(Some(timeout))
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .user_agent("SolidCraft")
        .max_redirects(0)
        .build()
        .into()
}

impl Moonraker {
    /// A host for the printer at `url` (`http://printer.local`); the status thread starts at once.
    pub fn new(url: &str) -> Result<Moonraker, String> {
        let url = url.trim().trim_end_matches('/').to_string();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err("the printer address must start with http:// or https://".into());
        }
        if url.len() > 512 || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err("the printer address is not a URL".into());
        }
        let shared =
            Arc::new(Shared { url, jobs: Mutex::new(HashMap::new()), next_job: AtomicU64::new(0), snapshot: Mutex::new(Snapshot::default()) });
        let weak = Arc::downgrade(&shared);
        std::thread::Builder::new().name("moonraker-status".into()).spawn(move || poll_loop(&weak)).map_err(|e| e.to_string())?;
        Ok(Moonraker { shared })
    }
}

/// Polls while the host lives; drops out when the last `Moonraker` is gone.
fn poll_loop(weak: &Weak<Shared>) {
    loop {
        let Some(shared) = weak.upgrade() else { return };
        shared.poll();
        drop(shared);
        std::thread::sleep(POLL_EVERY);
    }
}

fn read_answer(response: &mut ureq::http::Response<ureq::Body>) -> Result<(u16, String), String> {
    let status = response.status().as_u16();
    let text = response.body_mut().with_config().limit(ANSWER_LIMIT).read_to_string().map_err(|e| e.to_string())?;
    Ok((status, text))
}

fn short(text: &str) -> String {
    text.chars().take(200).collect()
}

impl Shared {
    fn set_job(&self, id: u64, state: JobState) {
        lock(&self.jobs).insert(id, state);
    }

    fn new_job(&self) -> u64 {
        let id = self.next_job.fetch_add(1, Ordering::SeqCst) + 1;
        self.set_job(id, JobState::Queued);
        id
    }

    /// One status request; its outcome lands in the snapshot either way.
    fn poll(&self) {
        let url = format!("{}/printer/objects/query?print_stats&display_status&extruder&heater_bed", self.url);
        let result = agent(POLL_TIMEOUT).get(&url).call().map_err(|e| e.to_string()).and_then(|mut r| {
            let (status, text) = read_answer(&mut r)?;
            if !(200..300).contains(&status) {
                return Err(format!("HTTP {status}: {}", short(&text)));
            }
            serde_json::from_str::<serde_json::Value>(&text).map_err(|e| e.to_string())
        });
        let mut snap = lock(&self.snapshot);
        match result {
            Ok(v) => {
                let st = &v["result"]["status"];
                let s = &mut snap.status;
                s.reachable = true;
                s.error = None;
                let text = |v: &serde_json::Value| v.as_str().unwrap_or("").to_string();
                let num = |v: &serde_json::Value| v.as_f64().filter(|x| x.is_finite()).unwrap_or(0.0);
                s.state = text(&st["print_stats"]["state"]);
                s.filename = text(&st["print_stats"]["filename"]);
                s.message = text(&st["print_stats"]["message"]);
                s.print_duration_s = num(&st["print_stats"]["print_duration"]);
                s.progress = num(&st["display_status"]["progress"]).clamp(0.0, 1.0);
                s.extruder = Heater { temperature: num(&st["extruder"]["temperature"]), target: num(&st["extruder"]["target"]) };
                s.bed = Heater { temperature: num(&st["heater_bed"]["temperature"]), target: num(&st["heater_bed"]["target"]) };
                snap.last_ok = Some(Instant::now());
            }
            Err(e) => {
                snap.status.reachable = false;
                snap.status.error = Some(e);
            }
        }
    }

    /// The multipart upload. `Ok(started)`: the printer took the file (and started it when asked).
    fn send_upload(&self, name: &str, gcode: &[u8], start: bool) -> Result<bool, String> {
        let boundary = format!("----SolidCraftUpload{:x}{:x}", std::process::id(), Instant::now().elapsed().as_nanos() ^ gcode.len() as u128);
        let mut body = Vec::with_capacity(gcode.len() + 512);
        for (field, value) in [("root", "gcodes"), ("print", if start { "true" } else { "false" })] {
            body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{field}\"\r\n\r\n{value}\r\n").as_bytes());
        }
        let file_name: String = name.chars().filter(|c| *c != '"' && !c.is_control()).collect();
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{file_name}\"\r\nContent-Type: application/octet-stream\r\n\r\n")
                .as_bytes(),
        );
        body.extend_from_slice(gcode);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let url = format!("{}/server/files/upload", self.url);
        let mut response =
            agent(UPLOAD_TIMEOUT).post(&url).header("Content-Type", &format!("multipart/form-data; boundary={boundary}")).send(&body[..]).map_err(
                |e| {
                    if start {
                        format!("no answer from the printer ({e}); it may or may not have started the print: check it before sending again")
                    } else {
                        format!("no answer from the printer ({e}); check it before sending again")
                    }
                },
            )?;
        let (status, text) = read_answer(&mut response)?;
        if !(200..300).contains(&status) {
            return Err(format!("the printer refused the upload: HTTP {status} {}", short(&text)));
        }
        let v: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
        Ok(v["print_started"].as_bool().unwrap_or(false))
    }

    fn send_cancel(&self) -> Result<(), String> {
        let url = format!("{}/printer/print/cancel", self.url);
        let mut response = agent(CANCEL_TIMEOUT).post(&url).send(&[][..]).map_err(|e| format!("no answer from the printer ({e})"))?;
        let (status, text) = read_answer(&mut response)?;
        if (200..300).contains(&status) { Ok(()) } else { Err(format!("the printer refused the cancel: HTTP {status} {}", short(&text))) }
    }
}

impl PrinterHost for Moonraker {
    fn info(&self) -> PrinterInfo {
        PrinterInfo { name: "Moonraker".into(), url: self.shared.url.clone() }
    }

    fn upload(&self, name: &str, gcode: Vec<u8>, start: bool) -> Result<u64, String> {
        if name.is_empty() || name.len() > 255 {
            return Err("the file name is empty or too long".into());
        }
        let id = self.shared.new_job();
        let shared = self.shared.clone();
        let name = name.to_string();
        let total = gcode.len() as u64;
        std::thread::Builder::new()
            .name("moonraker-upload".into())
            .spawn(move || {
                shared.set_job(id, JobState::Running { sent: 0, total });
                let state = match shared.send_upload(&name, &gcode, start) {
                    Ok(started) => JobState::Done { message: format!("{name} is on the printer"), started },
                    Err(message) => JobState::Failed { message },
                };
                shared.set_job(id, state);
            })
            .map_err(|e| e.to_string())?;
        Ok(id)
    }

    fn cancel_print(&self) -> Result<u64, String> {
        let id = self.shared.new_job();
        let shared = self.shared.clone();
        std::thread::Builder::new()
            .name("moonraker-cancel".into())
            .spawn(move || {
                shared.set_job(id, JobState::Running { sent: 0, total: 0 });
                let state = match shared.send_cancel() {
                    Ok(()) => JobState::Done { message: "cancel sent to the printer".into(), started: false },
                    Err(message) => JobState::Failed { message },
                };
                shared.set_job(id, state);
            })
            .map_err(|e| e.to_string())?;
        Ok(id)
    }

    fn job(&self, id: u64) -> Option<JobState> {
        lock(&self.shared.jobs).get(&id).cloned()
    }

    fn status(&self) -> PrinterStatus {
        let snap = lock(&self.shared.snapshot);
        let mut status = snap.status.clone();
        status.age_s = snap.last_ok.map(|t| t.elapsed().as_secs_f64());
        status
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_http_addresses_make_a_host() {
        assert!(Moonraker::new("printer.local").is_err());
        assert!(Moonraker::new("ftp://x").is_err());
        assert!(Moonraker::new("http://a b").is_err());
        let host = Moonraker::new("http://127.0.0.1:1/ ").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(host.info().url, "http://127.0.0.1:1");
        assert_eq!(host.job(1), None);
        let status = host.status();
        assert!(!status.reachable || status.age_s.is_some());
    }

    #[test]
    fn an_upload_to_nowhere_fails_as_a_job_rather_than_an_error() {
        let host = Moonraker::new("http://127.0.0.1:1").unwrap_or_else(|e| panic!("{e}"));
        let id = host.upload("part.gcode", b"G28\n".to_vec(), false).unwrap_or_else(|e| panic!("{e}"));
        let t = Instant::now();
        loop {
            match host.job(id) {
                Some(JobState::Failed { message }) => {
                    assert!(message.contains("no answer"), "{message}");
                    break;
                }
                Some(JobState::Done { .. }) => panic!("nothing listens on port 1"),
                _ if t.elapsed() > Duration::from_secs(30) => panic!("the job never finished"),
                _ => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        assert!(host.upload("", Vec::new(), false).is_err());
    }
}
