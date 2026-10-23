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

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::atomic::AtomicBool;

use solidcraft_engine::printer_host::{CameraFrame, Heater, JobState, PrinterHost, PrinterInfo, PrinterStatus, Webcam};

const POLL_EVERY: Duration = Duration::from_secs(2);
const POLL_TIMEOUT: Duration = Duration::from_secs(4);
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(180);
/// A cancel, a pause or a script waits in Klipper's queue behind the running macro (a start
/// sequence heating for minutes answers nothing until it ends), so the request waits with it;
/// a printer that is off still fails at the connect timeout.
const QUEUED_TIMEOUT: Duration = Duration::from_secs(20 * 60);
/// The webcam list is asked for on the first poll and then every so many polls.
const WEBCAM_POLLS: u64 = 30;
/// Most bytes one camera frame may be.
const FRAME_LIMIT: usize = 8 << 20;
/// A stall this long on the stream reconnects it.
const STREAM_READ_TIMEOUT: Duration = Duration::from_secs(10);
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
    polls: AtomicU64,
    camera: Mutex<Camera>,
}

#[derive(Default)]
struct Snapshot {
    status: PrinterStatus,
    last_ok: Option<Instant>,
    webcams: Vec<Webcam>,
}

/// The watched stream: which URL, the newest frame, and what went wrong.
#[derive(Default)]
struct Camera {
    url: Option<String>,
    /// Set while a reader thread should keep going; cleared to stop it.
    running: Option<Arc<AtomicBool>>,
    frame: Option<CameraFrame>,
    error: Option<String>,
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
        let shared = Arc::new(Shared {
            url,
            jobs: Mutex::new(HashMap::new()),
            next_job: AtomicU64::new(0),
            snapshot: Mutex::new(Snapshot::default()),
            polls: AtomicU64::new(0),
            camera: Mutex::new(Camera::default()),
        });
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

    /// The camera list, with relative stream addresses made absolute against the printer's URL.
    fn fetch_webcams(&self) -> Result<Vec<Webcam>, String> {
        let url = format!("{}/server/webcams/list", self.url);
        let mut r = agent(POLL_TIMEOUT).get(&url).call().map_err(|e| e.to_string())?;
        let (status, text) = read_answer(&mut r)?;
        if !(200..300).contains(&status) {
            return Err(format!("HTTP {status}: {}", short(&text)));
        }
        let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        let absolute = |s: &str| {
            if s.starts_with("http://") || s.starts_with("https://") { s.to_string() } else { format!("{}/{}", self.url, s.trim_start_matches('/')) }
        };
        Ok(v["result"]["webcams"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|w| Webcam {
                name: w["name"].as_str().unwrap_or("Camera").to_string(),
                stream_url: w["stream_url"].as_str().map(absolute).unwrap_or_default(),
                snapshot_url: w["snapshot_url"].as_str().map(absolute).unwrap_or_default(),
                enabled: w["enabled"].as_bool().unwrap_or(true),
                rotation: u16::try_from(w["rotation"].as_u64().unwrap_or(0) % 360).unwrap_or(0),
                flip_horizontal: w["flip_horizontal"].as_bool().unwrap_or(false),
                flip_vertical: w["flip_vertical"].as_bool().unwrap_or(false),
            })
            .filter(|w| !w.stream_url.is_empty())
            .collect())
    }

    /// One status request; its outcome lands in the snapshot either way.
    fn poll(&self) {
        let n = self.polls.fetch_add(1, Ordering::SeqCst);
        if n.is_multiple_of(WEBCAM_POLLS)
            && let Ok(list) = self.fetch_webcams()
        {
            lock(&self.snapshot).webcams = list;
        }
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

    fn send_script(&self, gcode: &str) -> Result<(), String> {
        let url = format!("{}/printer/gcode/script", self.url);
        let body = serde_json::json!({ "script": gcode }).to_string();
        let mut response = agent(QUEUED_TIMEOUT)
            .post(&url)
            .header("Content-Type", "application/json")
            .send(body.as_bytes())
            .map_err(|e| format!("no answer from the printer ({e})"))?;
        let (status, text) = read_answer(&mut response)?;
        if (200..300).contains(&status) { Ok(()) } else { Err(format!("the printer refused `{gcode}`: HTTP {status} {}", short(&text))) }
    }

    fn send_cancel(&self) -> Result<(), String> {
        let url = format!("{}/printer/print/cancel", self.url);
        let mut response = agent(QUEUED_TIMEOUT).post(&url).send(&[][..]).map_err(|e| format!("no answer from the printer ({e})"))?;
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

    fn webcams(&self) -> Vec<Webcam> {
        lock(&self.shared.snapshot).webcams.clone()
    }

    fn script(&self, gcode: &str) -> Result<u64, String> {
        let gcode = gcode.trim();
        if gcode.is_empty() || gcode.len() > 4096 || gcode.chars().any(char::is_control) {
            return Err("the script must be one line of G-code".into());
        }
        let id = self.shared.new_job();
        let shared = self.shared.clone();
        let gcode = gcode.to_string();
        std::thread::Builder::new()
            .name("moonraker-script".into())
            .spawn(move || {
                shared.set_job(id, JobState::Running { sent: 0, total: 0 });
                let state = match shared.send_script(&gcode) {
                    Ok(()) => JobState::Done { message: format!("{gcode} sent"), started: false },
                    Err(message) => JobState::Failed { message },
                };
                shared.set_job(id, state);
            })
            .map_err(|e| e.to_string())?;
        Ok(id)
    }

    fn watch_camera(&self, url: &str, on: bool) {
        let mut camera = lock(&self.shared.camera);
        let same = camera.url.as_deref() == Some(url) && camera.running.as_ref().is_some_and(|r| r.load(Ordering::SeqCst));
        if on && same {
            return;
        }
        if let Some(running) = camera.running.take() {
            running.store(false, Ordering::SeqCst);
        }
        camera.frame = None;
        camera.error = None;
        camera.url = on.then(|| url.to_string());
        if !on {
            return;
        }
        let running = Arc::new(AtomicBool::new(true));
        camera.running = Some(running.clone());
        let shared = self.shared.clone();
        let url = url.to_string();
        if let Err(e) = std::thread::Builder::new().name("moonraker-camera".into()).spawn(move || stream_loop(&shared, &url, &running)) {
            camera.error = Some(e.to_string());
            camera.running = None;
        }
    }

    fn camera_frame(&self) -> Result<Option<CameraFrame>, String> {
        let camera = lock(&self.shared.camera);
        match (&camera.frame, &camera.error) {
            (Some(frame), _) => Ok(Some(frame.clone())),
            (None, Some(e)) => Err(e.clone()),
            (None, None) => Ok(None),
        }
    }

    fn status(&self) -> PrinterStatus {
        let snap = lock(&self.shared.snapshot);
        let mut status = snap.status.clone();
        status.age_s = snap.last_ok.map(|t| t.elapsed().as_secs_f64());
        status
    }
}

/// Reads the MJPEG stream at `url` until `running` clears, reconnecting after a failure.
fn stream_loop(shared: &Shared, url: &str, running: &AtomicBool) {
    let mut sequence = 0u64;
    while running.load(Ordering::SeqCst) {
        let outcome = stream_once(url, running, &mut |jpeg| {
            sequence += 1;
            let mut camera = lock(&shared.camera);
            camera.frame = Some(CameraFrame { sequence, jpeg: Arc::new(jpeg) });
            camera.error = None;
        });
        if !running.load(Ordering::SeqCst) {
            return;
        }
        if let Err(e) = outcome {
            lock(&shared.camera).error = Some(e);
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// One connection to the stream: an HTTP/1.0 request (so the answer is never chunked), then
/// the multipart body, one JPEG per part, until the connection ends or `running` clears.
fn stream_once(url: &str, running: &AtomicBool, frame: &mut dyn FnMut(Vec<u8>)) -> Result<(), String> {
    let rest = url.strip_prefix("http://").ok_or("only http:// camera streams are supported")?;
    let (host_port, path) = rest.split_once('/').map_or((rest, "/".to_string()), |(h, p)| (h, format!("/{p}")));
    let address = if host_port.contains(':') { host_port.to_string() } else { format!("{host_port}:80") };
    let socket =
        std::net::ToSocketAddrs::to_socket_addrs(&address).map_err(|e| e.to_string())?.next().ok_or("the camera's address does not resolve")?;
    let mut stream = TcpStream::connect_timeout(&socket, CONNECT_TIMEOUT).map_err(|e| format!("camera: {e}"))?;
    stream.set_read_timeout(Some(STREAM_READ_TIMEOUT)).map_err(|e| e.to_string())?;
    stream
        .write_all(format!("GET {path} HTTP/1.0\r\nHost: {host_port}\r\nUser-Agent: SolidCraft\r\nConnection: close\r\n\r\n").as_bytes())
        .map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).map_err(|e| e.to_string())?;
    if !line.contains(" 200") {
        return Err(format!("camera: {}", line.trim()));
    }
    let mut boundary = None;
    loop {
        line.clear();
        if reader.read_line(&mut line).map_err(|e| e.to_string())? == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':')
            && k.eq_ignore_ascii_case("content-type")
        {
            boundary = v.split("boundary=").nth(1).map(|b| b.trim().trim_matches('"').to_string());
        }
    }
    let boundary = boundary.ok_or("the camera did not send a multipart stream")?;
    while running.load(Ordering::SeqCst) {
        let Some(jpeg) = next_part(&mut reader, &boundary)? else { return Ok(()) };
        if !jpeg.is_empty() {
            frame(jpeg);
        }
    }
    Ok(())
}

/// The next part's body: skips to the boundary, reads the part headers, then the body by its
/// Content-Length, or up to the next boundary line when there is none. `None` at the stream's end.
fn next_part(reader: &mut impl BufRead, boundary: &str) -> Result<Option<Vec<u8>>, String> {
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            return Ok(None);
        }
        if line.trim().trim_start_matches("--") == boundary {
            break;
        }
    }
    let mut length: Option<usize> = None;
    loop {
        line.clear();
        if reader.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            return Ok(None);
        }
        if line.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':')
            && k.eq_ignore_ascii_case("content-length")
        {
            length = v.trim().parse().ok();
        }
    }
    match length {
        Some(n) if n > FRAME_LIMIT => Err(format!("a camera frame of {n} bytes is past the {FRAME_LIMIT} byte limit")),
        Some(n) => {
            let mut body = vec![0u8; n];
            reader.read_exact(&mut body).map_err(|e| e.to_string())?;
            Ok(Some(body))
        }
        None => {
            let marker = format!("--{boundary}");
            let mut body = Vec::new();
            loop {
                let mut chunk = Vec::new();
                if reader.read_until(b'\n', &mut chunk).map_err(|e| e.to_string())? == 0 {
                    return Ok(if body.is_empty() { None } else { Some(strip_crlf(body)) });
                }
                if String::from_utf8_lossy(&chunk).trim() == marker {
                    return Ok(Some(strip_crlf(body)));
                }
                body.extend_from_slice(&chunk);
                if body.len() > FRAME_LIMIT {
                    return Err("a camera frame is past the size limit".into());
                }
            }
        }
    }
}

fn strip_crlf(mut body: Vec<u8>) -> Vec<u8> {
    while body.last().is_some_and(|b| *b == b'\r' || *b == b'\n') {
        body.pop();
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multipart_parts_come_out_one_jpeg_at_a_time() {
        let a = b"\xff\xd8frame-one\xff\xd9";
        let b = b"\xff\xd8frame-two\xff\xd9";
        let mut data = Vec::new();
        data.extend_from_slice(b"--boundarydonotcross\r\nContent-Type: image/jpeg\r\nContent-Length: 13\r\nX-Timestamp: 1.0\r\n\r\n");
        data.extend_from_slice(a);
        data.extend_from_slice(b"\r\n--boundarydonotcross\r\nContent-Type: image/jpeg\r\n\r\n");
        data.extend_from_slice(b);
        data.extend_from_slice(b"\r\n--boundarydonotcross\r\n");
        let mut reader = std::io::Cursor::new(data);
        assert_eq!(next_part(&mut reader, "boundarydonotcross").unwrap_or_else(|e| panic!("{e}")).as_deref(), Some(&a[..]));
        // Without a length the body runs to the next boundary line; that line is consumed.
        assert_eq!(next_part(&mut reader, "boundarydonotcross").unwrap_or_else(|e| panic!("{e}")).as_deref(), Some(&b[..]));
        assert_eq!(next_part(&mut reader, "boundarydonotcross").unwrap_or_else(|e| panic!("{e}")), None);
        let mut huge = std::io::Cursor::new(b"--b\r\nContent-Length: 99999999\r\n\r\n".to_vec());
        assert!(next_part(&mut huge, "b").is_err());
    }

    #[test]
    fn a_stream_to_nowhere_fails_cleanly() {
        let running = AtomicBool::new(true);
        assert!(stream_once("http://127.0.0.1:1/webcam/?action=stream", &running, &mut |_| {}).is_err());
        assert!(stream_once("https://x/", &running, &mut |_| {}).is_err());
    }

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
