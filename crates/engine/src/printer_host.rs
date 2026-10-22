//! The printer the `print.*` commands talk to. The engine knows no network: the application
//! installs a [`PrinterHost`] (the desktop app's Moonraker client), and a build without one
//! (the browser, tests without a fake) answers "no printer is set up". Everything a host does on
//! the network runs off the command thread: an upload or a cancel is a job the host runs and
//! reports through [`PrinterHost::job`], and [`PrinterHost::status`] hands back what the host
//! last heard without waiting, so a printer that has gone quiet never stalls the engine.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// The printer a host speaks to, for the panel and the control channel.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PrinterInfo {
    pub name: String,
    pub url: String,
}

/// Where a job stands: an upload or a cancel the host runs in the background.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running {
        sent: u64,
        total: u64,
    },
    Done {
        message: String,
        /// The printer started printing the file (an upload asked to).
        started: bool,
    },
    Failed {
        message: String,
    },
}

impl JobState {
    pub fn finished(&self) -> bool {
        matches!(self, JobState::Done { .. } | JobState::Failed { .. })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Heater {
    pub temperature: f64,
    pub target: f64,
}

/// What the host last heard from the printer. The fields keep their last values while the
/// printer is unreachable, so a panel can show where things stood and how old that is.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PrinterStatus {
    /// The last request was answered; false while the printer cannot be reached.
    pub reachable: bool,
    /// Seconds since the last good answer; None before the first.
    pub age_s: Option<f64>,
    /// Why the printer is unreachable, when it is.
    pub error: Option<String>,
    /// Klipper's print state: `standby`, `printing`, `paused`, `complete`, `cancelled`, `error`;
    /// empty before the first answer.
    pub state: String,
    pub filename: String,
    /// 0 to 1.
    pub progress: f64,
    pub print_duration_s: f64,
    pub extruder: Heater,
    pub bed: Heater,
    /// The printer's own message, if any.
    pub message: String,
}

/// A camera the printer offers (Moonraker's webcam list).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Webcam {
    pub name: String,
    /// Absolute URL of the MJPEG stream.
    pub stream_url: String,
    /// Absolute URL of a single JPEG, when the camera offers one.
    pub snapshot_url: String,
    pub enabled: bool,
    /// Degrees clockwise, 0, 90, 180 or 270.
    pub rotation: u16,
    pub flip_horizontal: bool,
    pub flip_vertical: bool,
}

/// One frame of a camera stream: JPEG bytes and a number that grows with every new frame.
#[derive(Clone, Debug)]
pub struct CameraFrame {
    pub sequence: u64,
    pub jpeg: Arc<Vec<u8>>,
}

/// A printer the application can reach. Every method returns at once.
pub trait PrinterHost: Send + Sync {
    fn info(&self) -> PrinterInfo;
    /// The cameras the printer lists; empty before the first answer or without any.
    fn webcams(&self) -> Vec<Webcam>;
    /// Run a G-code script on the printer (a temperature, a pause); a job to follow.
    fn script(&self, gcode: &str) -> Result<u64, String>;
    /// Start (`on`) or stop reading the MJPEG stream at `url`; frames come through
    /// [`PrinterHost::camera_frame`]. Stopping drops the connection.
    fn watch_camera(&self, url: &str, on: bool);
    /// The newest frame of the watched stream, and why there is none when there is none.
    fn camera_frame(&self) -> Result<Option<CameraFrame>, String>;
    /// Start sending `gcode` to the printer as the file `name`, to be printed at once when
    /// `start`. The job id reports progress through [`PrinterHost::job`].
    fn upload(&self, name: &str, gcode: Vec<u8>, start: bool) -> Result<u64, String>;
    /// Ask the printer to cancel the running print.
    fn cancel_print(&self) -> Result<u64, String>;
    /// Where a job stands; None for an id this host never gave out.
    fn job(&self, id: u64) -> Option<JobState>;
    /// The latest status, without waiting on the network.
    fn status(&self) -> PrinterStatus;
}

impl crate::Session {
    /// The printer host is installed by the application, never by a design or a command.
    pub fn set_printer_host(&mut self, host: Option<Arc<dyn PrinterHost>>) {
        self.printer_host = host;
    }

    pub fn printer_host(&self) -> Option<&Arc<dyn PrinterHost>> {
        self.printer_host.as_ref()
    }
}

/// A file name the printer's file store accepts: the stem of `name` with path separators and
/// control characters replaced, ending in `.gcode`, at most 120 characters.
pub fn gcode_file_name(name: &str) -> String {
    let stem = name.trim().trim_end_matches(".gcode").trim_end_matches(".GCODE");
    let mut out: String = stem
        .chars()
        .map(|c| if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c })
        .take(120)
        .collect();
    if out.trim_matches(|c: char| c == '_' || c == '.' || c.is_whitespace()).is_empty() {
        out = "design".into();
    }
    format!("{out}.gcode")
}
