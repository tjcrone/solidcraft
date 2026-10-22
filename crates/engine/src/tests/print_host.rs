use std::sync::{Arc, Mutex};

use super::*;
use crate::printer_host::{Heater, JobState, PrinterHost, PrinterInfo, PrinterStatus, gcode_file_name};

/// A printer that records what it was asked and answers from a script.
#[derive(Default)]
struct Fake {
    uploads: Mutex<Vec<(String, usize, bool)>>,
    cancels: Mutex<u32>,
    jobs: Mutex<Vec<JobState>>,
    status: Mutex<PrinterStatus>,
    refuse: Mutex<bool>,
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
    fn job(&self, id: u64) -> Option<JobState> {
        self.jobs.lock().unwrap().get(id.checked_sub(1)? as usize).cloned()
    }
    fn status(&self) -> PrinterStatus {
        self.status.lock().unwrap().clone()
    }
}

const KLIPPER_02: &str = r#"{"layer_height": 0.2, "initial_layer_print_height": 0.2, "gcode_flavor": "klipper"}"#;

fn sliced_session(host: &Arc<Fake>) -> Session {
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
fn printer_file_names_are_tamed() {
    assert_eq!(gcode_file_name("Bracket"), "Bracket.gcode");
    assert_eq!(gcode_file_name("Bracket.gcode"), "Bracket.gcode");
    assert_eq!(gcode_file_name("  "), "design.gcode");
    assert_eq!(gcode_file_name("a/b\\c:d*e?f\"g<h>i|j"), "a_b_c_d_e_f_g_h_i_j.gcode");
    assert_eq!(gcode_file_name(&"x".repeat(300)).len(), 126);
    assert_eq!(gcode_file_name("tab\there"), "tab_here.gcode");
}
