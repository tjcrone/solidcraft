//! Slicing for 3D printing: triangle meshes in, G-code out. The [`Slicer`] trait is what the rest
//! of SolidCraft talks to; [`SxSlicer`] is the one implementation today, SlicerX's `sx-core`
//! engine. Settings are a JSON object of OrcaSlicer key names (`layer_height`, `wall_loops`,
//! `sparse_infill_density`, `gcode_flavor`, …); keys left out keep the engine's defaults.
//!
//! Units are millimetres, Z up, like the rest of SolidCraft. Everything here is pure Rust with no
//! file system, network or threads of its own, so it builds for wasm too.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![forbid(unsafe_code)]

pub mod file_beads;
pub mod reach;
mod sx;
pub mod toolpaths;

pub use sx::SxSlicer;
pub use toolpaths::{Feature, FileBead, Layer, Path, Toolpaths};

/// Most triangles one job may carry: a design that tessellates past this is refused, not sliced.
pub const MAX_TRIANGLES: usize = 50_000_000;
/// Most bytes of settings JSON one job may carry.
pub const MAX_CONFIG_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("part {name}: {reason}")]
    Part { name: String, reason: String },
    #[error("settings: {0}")]
    Config(String),
    #[error("slicing failed: {0}")]
    Slice(String),
    #[error("writing G-code: {0}")]
    Gcode(String),
    /// The toolpaths or the finished file would run outside the printer's volume; nothing is written.
    #[error("refused, the print would run outside the printer's reach: {0}")]
    Unsafe(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// One printable body: a closed triangle mesh in millimetres, triangles counter-clockwise seen
/// from outside.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Part {
    pub name: String,
    pub positions: Vec<[f32; 3]>,
    pub triangles: Vec<[u32; 3]>,
}

impl Part {
    /// Refuses a mesh a slicer must not see: an index past the vertex list, a coordinate that is
    /// not a finite number, or more triangles than [`MAX_TRIANGLES`].
    pub fn validate(&self) -> Result<()> {
        let bad = |reason: String| Error::Part { name: self.name.clone(), reason };
        if self.triangles.len() > MAX_TRIANGLES {
            return Err(bad(format!("{} triangles is more than the {MAX_TRIANGLES} a job may carry", self.triangles.len())));
        }
        if let Some(p) = self.positions.iter().find(|p| !p.iter().all(|c| c.is_finite())) {
            return Err(bad(format!("vertex {p:?} is not a finite point")));
        }
        let n = self.positions.len();
        if let Some(t) = self.triangles.iter().find(|t| t.iter().any(|&i| i as usize >= n)) {
            return Err(bad(format!("triangle {t:?} names a vertex past the {n} there are")));
        }
        Ok(())
    }
}

/// What one slice runs on: the parts that print, in the same space, and the settings as a JSON
/// object of OrcaSlicer keys.
#[derive(Clone, Debug)]
pub struct Job<'a> {
    pub parts: &'a [Part],
    /// A JSON object text, `{}` for the engine's defaults.
    pub config: &'a str,
}

/// A setting the engine could not use: the setting kept its default.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Refused {
    pub key: String,
    /// The value as JSON text.
    pub value: String,
    pub reason: String,
}

/// What the engine had to say about the settings and the slice; none of it stopped the job.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    pub refused: Vec<Refused>,
    /// Setting keys this engine never reads, sorted.
    pub ignored: Vec<String>,
    /// Settings lowered to the machine's limits, toolpath findings, and the engine's own warnings.
    pub warnings: Vec<String>,
}

/// A finished slice: the G-code and what the engine estimated for it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sliced {
    pub gcode: Vec<u8>,
    pub layers: u32,
    /// Estimated print time, seconds.
    pub time_s: f64,
    /// Filament over every slot, millimetres and grams.
    pub filament_mm: f64,
    pub filament_g: f64,
    pub report: Report,
    /// The paths the G-code was written from, for a layer preview.
    pub toolpaths: Toolpaths,
    /// The printer's bed as the settings describe it: `[min_x, min_y, max_x, max_y]` mm.
    pub bed: [f32; 4],
    /// The printer's printable height, mm.
    pub printable_height: f32,
    /// Where the slicer put the parts on the bed: plate coordinates = part coordinates + offset.
    /// The toolpaths and the G-code are in plate coordinates.
    pub offset: [f32; 3],
}

/// A slicing engine. One job in, one G-code file out; no state between calls.
pub trait Slicer {
    /// The engine's name and version, for the G-code header and the UI.
    fn name(&self) -> String;

    fn slice(&self, job: &Job) -> Result<Sliced>;
}

/// The slicer SolidCraft prints with.
pub fn default_slicer() -> impl Slicer {
    SxSlicer
}

#[cfg(test)]
#[path = "slice_tests.rs"]
mod tests;

/// True when the slicer reads `key` (an OrcaSlicer setting name) in some way, so a profile or a
/// design that sets it changes the print. The browser build cannot tell and says yes.
pub fn engine_reads(key: &str) -> bool {
    sx_core::config::is_used_key(key)
}
