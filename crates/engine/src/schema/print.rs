//! Contracts and result types for the commands in `cmd/print.rs`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Print the session's last `print.slice`: upload its G-code to the installed printer and start
/// printing it when the upload finishes (`print.upload` with `start: true`). Refused while the
/// design has changed since the slice, and without a printer. The upload runs in the background;
/// `print.job` reports its progress.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct StartParams {
    /// The file name on the printer; `.gcode` is added when it is missing, and characters a
    /// printer may refuse are replaced. Default: the design's name.
    #[serde(default)]
    pub name: Option<String>,
}

/// What `print.start` returns.
#[derive(Debug, Serialize, JsonSchema)]
pub struct StartResult {
    /// The upload job: `print.job` follows it.
    pub job: u64,
    /// The file name on the printer.
    pub name: String,
    /// The G-code's size, in bytes.
    pub bytes: u64,
    /// Always true: the printer starts the file once it has it.
    pub start: bool,
    /// In the warn parameter-check mode: what the call's parameters got wrong.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub param_warnings: Vec<String>,
}

#[cfg(test)]
#[path = "print_tests.rs"]
mod tests;
