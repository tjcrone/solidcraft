//! The SlicerX engine (`sx-core`, Apache-2.0) behind the [`Slicer`] trait. Everything the engine
//! does with the job happens in memory; this file only translates types and errors.

use sx_core::api::{self, Mesh, MeshPart, Plate, PrintConfig, SliceOptions};

use crate::{Error, Job, MAX_CONFIG_BYTES, Part, Result, Sliced, Slicer};

/// The engine release the workspace pins (`sx-core`'s `tag` in the root Cargo.toml); the crate
/// itself does not say its version. A test keeps the two in step.
pub const ENGINE_VERSION: &str = "0.1.9";

/// SlicerX's `sx-core` engine.
#[derive(Clone, Copy, Debug, Default)]
pub struct SxSlicer;

impl Slicer for SxSlicer {
    fn name(&self) -> String {
        format!("SlicerX sx-core {ENGINE_VERSION}")
    }

    fn slice(&self, job: &Job) -> Result<Sliced> {
        if job.config.len() > MAX_CONFIG_BYTES {
            return Err(Error::Config(format!("{} bytes of settings is more than the {MAX_CONFIG_BYTES} a job may carry", job.config.len())));
        }
        let mut config = PrintConfig::from_json(job.config.as_bytes()).map_err(|e| Error::Config(e.to_string()))?;
        // The same limits SlicerX's own command line applies: settings past the nozzle and machine
        // limits are lowered, and a setting no printer can run refuses the slice.
        let issues = sx_core::preflight::clamp_config(&mut config, &sx_core::gcode_lint::Limits::default());
        if sx_core::preflight::blocks(&issues) {
            return Err(Error::Config(sx_core::preflight::blocking_text(&issues)));
        }
        let parts = job.parts.iter().map(part).collect::<Result<Vec<_>>>()?;
        if parts.is_empty() {
            return Err(Error::Part { name: String::new(), reason: "nothing to print".into() });
        }
        let mesh = Mesh { name: "solidcraft".into(), parts, ..Mesh::default() };
        // The parts go to the middle of the profile's bed, as the desktop slicers do.
        let plate = Plate::single_on(mesh, config.bed_rect(), config.printable_height);
        let out = api::slice(&plate, &config, &SliceOptions::default()).map_err(|e| Error::Slice(e.to_string()))?;
        let mut gcode = Vec::new();
        let stats = api::emit_gcode(&out, &config, config.gcode_flavor, &mut gcode).map_err(|e| Error::Gcode(e.to_string()))?;
        // What `sx slice` does after emitting: the time estimate from the printer's motion limits
        // (M73 progress lines, the footer's estimated time), the profile's G-code substitutions, and
        // line numbers when the profile asks for them.
        let (gcode, _timing) = sx_core::firmware::finalize_timed(&gcode, None, Some(&config));
        let time_s = sx_core::firmware::footer_time(&gcode).unwrap_or(stats.time_s);
        let (gcode, _skipped) = sx_core::firmware::substitute(gcode, &config).map_err(|e| Error::Gcode(e.to_string()))?;
        let gcode = if sx_core::firmware::truthy(&config, "gcode_add_line_number") && !sx_core::firmware::truthy(&config, "binary_gcode") {
            sx_core::firmware::number_lines(&gcode)
        } else {
            gcode
        };
        Ok(Sliced { gcode, layers: out.layer_count, time_s, filament_mm: stats.filament_mm.iter().sum(), filament_g: stats.filament_g.iter().sum() })
    }
}

fn part(p: &Part) -> Result<MeshPart> {
    p.validate()?;
    Ok(MeshPart { name: p.name.clone(), slot: 1, positions: p.positions.clone(), triangles: p.triangles.clone(), ..MeshPart::default() })
}
