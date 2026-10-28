//! The SlicerX engine (`sx-core`, Apache-2.0) behind the [`Slicer`] trait. Everything the engine
//! does with the job happens in memory; this file translates types and errors, and runs the
//! engine's own safety checks before and after the slice so nothing that could reach outside the
//! printer's volume is ever written.

use serde_json::Value;
use sx_core::api::{self, Mesh, MeshPart, Plate, PrintConfig, SliceOptions};
use sx_core::preflight;

use crate::toolpaths::{Feature, Layer, Path, Toolpaths};
use crate::{Error, Job, MAX_CONFIG_BYTES, Part, Refused, Report, Result, Sliced, Slicer};

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
        let mut report = Report::default();
        let mut config = read_config(job.config, &mut report)?;
        // The same limits SlicerX's own command line applies: settings past the nozzle and machine
        // limits are lowered, and a setting no printer can run refuses the slice.
        let issues = preflight::clamp_config(&mut config, &sx_core::gcode_lint::Limits::default());
        if preflight::blocks(&issues) {
            return Err(Error::Config(preflight::blocking_text(&issues)));
        }
        report.warnings.extend(issues.iter().map(|i| i.message.clone()));
        if !(job.placement.x.is_finite() && job.placement.y.is_finite() && job.placement.rotation_deg.is_finite()) {
            return Err(Error::Part { name: String::new(), reason: "the placement is not finite".into() });
        }
        let mut parts = job.parts.iter().map(part).collect::<Result<Vec<_>>>()?;
        if parts.is_empty() {
            return Err(Error::Part { name: String::new(), reason: "nothing to print".into() });
        }
        // A turn about Z, about the footprint's centre, before the bed centres the footprint.
        if job.placement.rotation_deg != 0.0
            && let Some(c) = crate::footprint_centre(job.parts)
        {
            let (sin, cos) = job.placement.rotation_deg.to_radians().sin_cos();
            for p in parts.iter_mut().flat_map(|p| p.positions.iter_mut()) {
                let (dx, dy) = (p[0] - c[0], p[1] - c[1]);
                p[0] = c[0] + dx * cos - dy * sin;
                p[1] = c[1] + dx * sin + dy * cos;
            }
        }
        let mesh = Mesh { name: "solidcraft".into(), parts, ..Mesh::default() };
        // The parts go to the middle of the profile's bed, as the desktop slicers do, and from
        // there to where the placement asks.
        let mut plate = Plate::single_on(mesh, config.bed_rect(), config.printable_height);
        for object in &mut plate.objects {
            object.transform[12] += job.placement.x;
            object.transform[13] += job.placement.y;
        }
        let offset = plate.objects.first().map_or([0.0; 3], |o| [o.transform[12], o.transform[13], o.transform[14]]);
        let out = api::slice(&plate, &config, &SliceOptions::default()).map_err(|e| match e {
            api::Error::Blocked(why) | api::Error::Clearance(why) => Error::Unsafe(why),
            other => Error::Slice(other.to_string()),
        })?;
        // Nothing may run off the bed, into an excluded zone or over the printable height.
        let issues = preflight::check_toolpaths(&out, &config);
        if preflight::blocks(&issues) {
            return Err(Error::Unsafe(preflight::blocking_text(&issues)));
        }
        report.warnings.extend(issues.iter().map(|i| i.message.clone()));
        report.warnings.extend(out.warnings.iter().map(|w| w.message.clone()));
        let mut gcode = Vec::new();
        let stats = api::emit_gcode(&out, &config, config.gcode_flavor, &mut gcode).map_err(|e| match e {
            // The engine vets the templates too: an end that leaves a heater on, and the like.
            api::Error::Blocked(why) => Error::Unsafe(why),
            other => Error::Gcode(other.to_string()),
        })?;
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
        // The finished bytes, templates included, read back the way a printer would move: every
        // position inside the bed and under the printable height.
        let outside = crate::reach::check(&gcode, config.bed_rect(), config.printable_height);
        if outside.count > 0 {
            return Err(Error::Unsafe(format!(
                "{} move(s) in the finished G-code leave the printer's volume: {}",
                outside.count,
                outside.first.join("; ")
            )));
        }
        let bed: [f32; 4] = config.bed_rect().map(|v| v as f32);
        let mut toolpaths = toolpaths(&out);
        toolpaths.file_beads = crate::file_beads::read(&gcode, bed, config.filament_diameter as f32);
        Ok(Sliced {
            gcode,
            layers: out.layer_count,
            time_s,
            filament_mm: stats.filament_mm.iter().sum(),
            filament_g: stats.filament_g.iter().sum(),
            report,
            toolpaths,
            bed,
            printable_height: config.printable_height as f32,
            offset,
        })
    }
}

/// The engine's toolpaths in millimetres: its points are integers in units of `1 / SCALE` mm.
fn toolpaths(out: &api::SliceOutput) -> Toolpaths {
    let mm = |v: i32| (f64::from(v) / sx_core::geom::SCALE) as f32;
    let layers = out
        .layers
        .iter()
        .map(|l| Layer {
            z: l.z,
            height: l.height,
            time_s: l.time_s,
            paths: l
                .paths
                .iter()
                .filter_map(|p| {
                    let points: Vec<[f32; 2]> = l.path_points(p).iter().map(|q| [mm(q.x), mm(q.y)]).collect();
                    (points.len() >= 2).then(|| Path {
                        feature: feature(p.feature),
                        tool: p.tool,
                        width_mm: p.width_mm,
                        speed_mm_s: p.speed_mm_s,
                        points,
                    })
                })
                .collect(),
        })
        .collect();
    Toolpaths { layers, file_beads: Vec::new() }
}

fn feature(f: api::Feature) -> Option<Feature> {
    Some(match f {
        api::Feature::OuterWall => Feature::OuterWall,
        api::Feature::InnerWall => Feature::InnerWall,
        api::Feature::OverhangWall => Feature::OverhangWall,
        api::Feature::TopSurface => Feature::TopSurface,
        api::Feature::BottomSurface => Feature::BottomSurface,
        api::Feature::InternalSolid => Feature::InternalSolid,
        api::Feature::SparseInfill => Feature::SparseInfill,
        api::Feature::Bridge => Feature::Bridge,
        api::Feature::InternalBridge => Feature::InternalBridge,
        api::Feature::Support => Feature::Support,
        api::Feature::SupportInterface => Feature::SupportInterface,
        api::Feature::Brim => Feature::Brim,
        api::Feature::Skirt => Feature::Skirt,
        api::Feature::Ironing => Feature::Ironing,
        api::Feature::GapFill => Feature::GapFill,
        api::Feature::PrimeTower => Feature::PrimeTower,
        api::Feature::Custom => Feature::Custom,
    })
}

/// The settings, read one key at a time: a key whose value the engine cannot use keeps its default
/// and is reported, as are the keys this engine never reads; only a file that is not a JSON object
/// of keys refuses the job.
/// The bed `[min_x, min_y, max_x, max_y]` and printable height the settings describe, without
/// slicing anything: what a placement is measured against.
pub fn bed_of(config: &str) -> Result<([f32; 4], f32)> {
    let mut report = Report::default();
    let config = read_config(config, &mut report)?;
    let b = config.bed_rect();
    Ok(([b[0] as f32, b[1] as f32, b[2] as f32, b[3] as f32], config.printable_height as f32))
}

fn read_config(text: &str, report: &mut Report) -> Result<PrintConfig> {
    if text.len() > MAX_CONFIG_BYTES {
        return Err(Error::Config(format!("{} bytes of settings is more than the {MAX_CONFIG_BYTES} a job may carry", text.len())));
    }
    let value: Value = serde_json::from_str(text).map_err(|e| Error::Config(e.to_string()))?;
    let Value::Object(map) = value else {
        return Err(Error::Config("the settings must be a JSON object of setting keys".into()));
    };
    let mut config = PrintConfig::default();
    // A new layer height gets matching support gaps unless they are given (as the engine's own
    // strict reader does).
    let mut incoming = map.clone();
    if incoming.contains_key("layer_height") {
        for key in ["support_top_z_distance", "support_bottom_z_distance"] {
            incoming.entry(key).or_insert_with(|| Value::String("auto".into()));
        }
    }
    let issues = config.apply_value_lenient(&Value::Object(incoming)).map_err(|e| Error::Config(e.to_string()))?;
    report.refused.extend(issues.into_iter().map(|i| Refused { key: i.key, value: i.value.to_string(), reason: i.reason }));
    let placeholders = sx_core::config::placeholder_keys(&map);
    report.ignored = map.keys().filter(|k| !sx_core::config::is_used_key(k) && !placeholders.contains(&***k)).cloned().collect();
    report.ignored.sort();
    Ok(config)
}

fn part(p: &Part) -> Result<MeshPart> {
    p.validate()?;
    Ok(MeshPart { name: p.name.clone(), slot: 1, positions: p.positions.clone(), triangles: p.triangles.clone(), ..MeshPart::default() })
}
