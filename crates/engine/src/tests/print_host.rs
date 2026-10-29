use std::sync::{Arc, Mutex};

use super::*;
use crate::printer_host::{CameraFrame, Heater, JobState, PrinterHost, PrinterInfo, PrinterStatus, Webcam, gcode_file_name};

/// A printer that records what it was asked and answers from a script.
#[derive(Default)]
pub(crate) struct Fake {
    pub(crate) uploads: Mutex<Vec<(String, usize, bool)>>,
    cancels: Mutex<u32>,
    jobs: Mutex<Vec<JobState>>,
    status: Mutex<PrinterStatus>,
    refuse: Mutex<bool>,
    scripts: Mutex<Vec<String>>,
    watching: Mutex<Option<String>>,
    timelapse: Mutex<Vec<bool>>,
}

impl PrinterHost for Fake {
    fn info(&self) -> PrinterInfo {
        PrinterInfo { name: "fake".into(), url: "http://fake".into() }
    }
    fn upload(&self, name: &str, gcode: Vec<u8>, start: bool) -> std::result::Result<u64, String> {
        if *self.refuse.lock().unwrap() {
            return Err("printer busy".into());
        }
        self.uploads.lock().unwrap().push((name.to_string(), gcode.len(), start));
        let mut jobs = self.jobs.lock().unwrap();
        jobs.push(JobState::Queued);
        Ok(jobs.len() as u64)
    }
    fn cancel_print(&self) -> std::result::Result<u64, String> {
        *self.cancels.lock().unwrap() += 1;
        let mut jobs = self.jobs.lock().unwrap();
        jobs.push(JobState::Done { message: "cancelled".into(), started: false });
        Ok(jobs.len() as u64)
    }
    fn set_timelapse(&self, enabled: bool) -> std::result::Result<u64, String> {
        self.timelapse.lock().unwrap().push(enabled);
        self.status.lock().unwrap().timelapse = Some(enabled);
        let mut jobs = self.jobs.lock().unwrap();
        jobs.push(JobState::Done { message: "timelapse".into(), started: false });
        Ok(jobs.len() as u64)
    }
    fn job(&self, id: u64) -> Option<JobState> {
        self.jobs.lock().unwrap().get(id.checked_sub(1)? as usize).cloned()
    }
    fn status(&self) -> PrinterStatus {
        self.status.lock().unwrap().clone()
    }
    fn webcams(&self) -> Vec<Webcam> {
        vec![Webcam { name: "Camera".into(), stream_url: "http://fake/webcam/?action=stream".into(), enabled: true, ..Default::default() }]
    }
    fn script(&self, gcode: &str) -> std::result::Result<u64, String> {
        if *self.refuse.lock().unwrap() {
            return Err("printer busy".into());
        }
        self.scripts.lock().unwrap().push(gcode.to_string());
        let mut jobs = self.jobs.lock().unwrap();
        jobs.push(JobState::Done { message: format!("ran {gcode}"), started: false });
        Ok(jobs.len() as u64)
    }
    fn watch_camera(&self, url: &str, on: bool) {
        *self.watching.lock().unwrap() = on.then(|| url.to_string());
    }
    fn camera_frame(&self) -> std::result::Result<Option<CameraFrame>, String> {
        Ok(None)
    }
}

const KLIPPER_02: &str = r#"{"layer_height": 0.2, "initial_layer_print_height": 0.2, "gcode_flavor": "klipper"}"#;

pub(crate) fn sliced_session(host: &Arc<Fake>) -> Session {
    let mut s = Session::default();
    s.set_printer_host(Some(host.clone()));
    run(&mut s, "solid.box", json!({"length": 10, "width": 10, "height": 10}));
    run(&mut s, "print.slice", json!({"settings": serde_json::from_str::<Value>(KLIPPER_02).unwrap()}));
    s
}

#[test]
fn without_a_printer_the_commands_say_so_and_status_is_not_an_error() {
    let mut s = Session::default();
    run(&mut s, "solid.box", json!({"length": 10, "width": 10, "height": 10}));
    run(&mut s, "print.slice", json!({"settings": serde_json::from_str::<Value>(KLIPPER_02).unwrap()}));
    assert!(s.execute("print.upload", &json!({})).is_err());
    assert!(s.execute("print.cancel", &json!({})).is_err());
    assert!(s.execute("print.job", &json!({"job": 1})).is_err());
    let st = run(&mut s, "print.status", json!({}));
    assert_eq!(st["installed"], false);
    assert_eq!(st["reachable"], false);
}

#[test]
fn an_upload_sends_the_held_gcode_under_a_safe_name_and_its_job_can_be_followed() {
    let host = Arc::new(Fake::default());
    let mut s = sliced_session(&host);
    let bytes = s.print_slice().unwrap().sliced.gcode.len();
    let r = run(&mut s, "print.upload", json!({}));
    assert_eq!(r["job"], 1);
    assert_eq!(r["name"], "Untitled.gcode", "{r}");
    assert_eq!(r["bytes"], bytes);
    assert_eq!(r["start"], false);
    let r = run(&mut s, "print.upload", json!({"name": "../evil/part:1.gcode", "start": true}));
    assert_eq!(r["name"], ".._evil_part_1.gcode", "{r}");
    assert_eq!(r["start"], true);
    let uploads = host.uploads.lock().unwrap().clone();
    assert_eq!(uploads, vec![("Untitled.gcode".into(), bytes, false), (".._evil_part_1.gcode".into(), bytes, true)]);
    // The job's states come through as the host reports them.
    let j = run(&mut s, "print.job", json!({"job": 1}));
    assert_eq!(j["state"], "queued");
    assert_eq!(j["finished"], false);
    host.jobs.lock().unwrap()[0] = JobState::Running { sent: 10, total: 100 };
    let j = run(&mut s, "print.job", json!({"job": 1}));
    assert_eq!(j["state"], "running");
    assert_eq!(j["sent"], 10);
    host.jobs.lock().unwrap()[0] = JobState::Done { message: "uploaded".into(), started: true };
    let j = run(&mut s, "print.job", json!({"job": 1}));
    assert_eq!(j["state"], "done");
    assert_eq!(j["started"], true);
    assert_eq!(j["finished"], true);
    assert!(s.execute("print.job", &json!({"job": 99})).is_err());
    assert!(s.execute("print.job", &json!({})).is_err());
}

#[test]
fn a_stale_slice_is_not_uploaded_and_a_refusing_printer_is_reported() {
    let host = Arc::new(Fake::default());
    let mut s = sliced_session(&host);
    run(&mut s, "solid.box", json!({"length": 5, "width": 5, "height": 5, "center": [30, 0, 0]}));
    let r = s.execute("print.upload", &json!({}));
    assert!(r.is_err(), "{r:?}");
    assert!(host.uploads.lock().unwrap().is_empty());
    run(&mut s, "edit.undo", json!({}));
    *host.refuse.lock().unwrap() = true;
    let r = s.execute("print.upload", &json!({}));
    assert!(r.is_err(), "{r:?}");
    assert!(s.execute("print.upload", &json!({"name": 7})).is_err());
}

#[test]
fn status_and_cancel_pass_through_the_host() {
    let host = Arc::new(Fake::default());
    let mut s = sliced_session(&host);
    *host.status.lock().unwrap() = PrinterStatus {
        reachable: true,
        age_s: Some(1.5),
        error: None,
        state: "printing".into(),
        filename: "cube.gcode".into(),
        progress: 0.25,
        print_duration_s: 120.0,
        extruder: Heater { temperature: 279.5, target: 280.0 },
        bed: Heater { temperature: 80.0, target: 80.0 },
        message: String::new(),
        timelapse: Some(true),
    };
    let st = run(&mut s, "print.status", json!({}));
    assert_eq!(st["installed"], true);
    assert_eq!(st["printer"]["name"], "fake");
    assert_eq!(st["state"], "printing");
    assert_eq!(st["progress"], 0.25);
    assert_eq!(st["extruder"]["target"], 280.0);
    let c = run(&mut s, "print.cancel", json!({}));
    assert_eq!(*host.cancels.lock().unwrap(), 1);
    assert_eq!(run(&mut s, "print.job", json!({"job": c["job"]}))["state"], "done");
    // The host survives a new design in the same session.
    run(&mut s, "file.new", json!({}));
    assert!(s.printer_host().is_some());
}

#[test]
fn temperatures_pauses_and_cameras_go_through_scripts_and_the_list() {
    let host = Arc::new(Fake::default());
    let mut s = sliced_session(&host);
    let r = run(&mut s, "print.heat", json!({"nozzle": 280, "bed": 80.4}));
    assert_eq!(r["jobs"].as_array().unwrap().len(), 2, "{r}");
    let r = run(&mut s, "print.heat", json!({"bed": 0}));
    assert_eq!(r["jobs"].as_array().unwrap().len(), 1);
    assert_eq!(host.scripts.lock().unwrap().clone(), vec!["M104 S280", "M140 S80", "M140 S0"]);
    for bad in [json!({}), json!({"nozzle": 400}), json!({"bed": -1}), json!({"nozzle": "hot"}), json!({"bed": f64::NAN})] {
        assert!(s.execute("print.heat", &bad).is_err(), "{bad}");
    }
    assert_eq!(host.scripts.lock().unwrap().len(), 3, "a refused request sends nothing");
    // Pause, Resume and Cancel follow what the printer says it is doing.
    assert!(s.execute("print.pause", &json!({})).is_err(), "nothing to pause on an unreachable printer");
    *host.status.lock().unwrap() = PrinterStatus { reachable: true, state: "standby".into(), ..Default::default() };
    assert!(s.execute("print.pause", &json!({})).is_err(), "nothing to pause while idle");
    assert!(s.execute("print.cancel", &json!({})).is_err(), "nothing to cancel while idle");
    host.status.lock().unwrap().state = "printing".into();
    run(&mut s, "print.pause", json!({}));
    assert!(s.execute("print.resume", &json!({})).is_err(), "not paused yet");
    host.status.lock().unwrap().state = "paused".into();
    run(&mut s, "print.resume", json!({}));
    assert_eq!(&host.scripts.lock().unwrap()[3..], ["PAUSE", "RESUME"]);
    let w = run(&mut s, "print.webcams", json!({}));
    assert_eq!(w["webcams"][0]["name"], "Camera");
    assert_eq!(w["webcams"][0]["stream_url"], "http://fake/webcam/?action=stream");
    let r = run(&mut s, "print.timelapse", json!({"enabled": false}));
    assert!(r["job"].as_u64().is_some(), "{r}");
    assert_eq!(run(&mut s, "print.status", json!({}))["timelapse"], false);
    for bad in [json!({}), json!({"enabled": "no"}), json!({"enabled": 0})] {
        assert!(s.execute("print.timelapse", &bad).is_err(), "{bad}");
    }
    assert_eq!(host.timelapse.lock().unwrap().clone(), vec![false], "a refused request sends nothing");
    let mut bare = Session::default();
    assert!(bare.execute("print.timelapse", &json!({"enabled": true})).is_err());
    assert!(bare.execute("print.heat", &json!({"bed": 60})).is_err());
    assert!(bare.execute("print.webcams", &json!({})).is_err());
}

#[test]
fn printer_file_names_are_tamed() {
    assert_eq!(gcode_file_name("Bracket"), "Bracket.gcode");
    assert_eq!(gcode_file_name("Bracket.gcode"), "Bracket.gcode");
    assert_eq!(gcode_file_name("  "), "design.gcode");
    assert_eq!(gcode_file_name("a/b\\c:d*e?f\"g<h>i|j"), "a_b_c_d_e_f_g_h_i_j.gcode");
    assert_eq!(gcode_file_name(&"x".repeat(300)).len(), 126);
    assert_eq!(gcode_file_name("tab\there"), "tab_here.gcode");
}
