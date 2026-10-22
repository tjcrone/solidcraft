//! Slicing the design for 3D printing (`print.*`): the bodies go to the slicer in
//! `solidcraft-manufacture` as welded meshes and come back as G-code. The session keeps the last
//! slice (`crate::held_slice`): `print.preview` reads its toolpaths for a layer preview and
//! `print.save` writes its G-code, so neither runs the engine again. There is no dialog; the Print
//! workspace, scripts, the CLI and the control channel all run these commands.

use serde_json::{Map, Value, json};
use solidcraft_manufacture::{Job, MAX_CONFIG_BYTES, Part, Slicer, Toolpaths};

use super::CommandSpec;
use crate::held_slice::HeldSlice;
use crate::params::{bad, str_, string_list};
use crate::{EngineError, Result, Session};

const SLICE: &str = "print.slice";
const PREVIEW: &str = "print.preview";
const SAVE: &str = "print.save";

/// Most points one `print.preview` answer carries for a layer, so a reply stays a reply.
const MAX_PREVIEW_POINTS: usize = 2_000_000;

pub static COMMANDS: &[CommandSpec] = &[
    CommandSpec::new(SLICE, "Slice for Printing", slice).noundo().at("PRINT", "SLICE").icon("mesh_export").params(
        "path?: a .gcode file to write as well; config?: path, or [paths], of JSON files of OrcaSlicer setting keys (printer, filament and process; no key may appear in two files; keys left out keep the slicer's defaults); settings?: {key: value} laid over the files'; bodies?: [names] (default: all) → path?, bytes, layers, segments, time_s, filament_mm, filament_g, slicer, refused: [{key, value, reason}] (settings the slicer could not use and left at their defaults), ignored: [keys the slicer never reads], warnings: [settings lowered to the machine's limits, toolpath findings]. The session keeps the slice for print.preview and print.save; a print that would leave the printer's volume is refused and nothing is kept or written",
    ),
    CommandSpec::new(PREVIEW, "Preview Toolpaths", preview).noundo().doc_only().icon("inspect").params(
        "layer?: index (0 = first) → stale (the design changed since the slice), slicer, layers, segments, time_s, filament_mm, filament_g, bed: [min_x, min_y, max_x, max_y], printable_height, offset: [x, y, z] (plate coordinates = design coordinates + offset; the toolpaths, bounds and bed are plate coordinates, as the G-code is), bounds: {min: [x, y, z], max: [x, y, z]}, z: [top of each layer], features: [{feature, label, paths, length_mm}]; with layer: layer: {index, z, height, time_s, paths: [{feature, tool, width_mm, speed_mm_s, points: [[x, y], …]}]}. Needs a print.slice in this session",
    ),
    CommandSpec::new(SAVE, "Save G-code", save).noundo().doc_only().at("PRINT", "SLICE").icon("export").params(
        "path: the .gcode file to write with the G-code of the session's last print.slice → path, bytes, stale (the design changed since the slice)",
    ),
];

fn slice(s: &mut Session, p: &Value) -> Result<Value> {
    let path = match p.get("path") {
        None | Some(Value::Null) => None,
        Some(Value::String(x)) if !x.trim().is_empty() && x.len() < 4096 => Some(x.as_str()),
        Some(_) => return Err(bad(SLICE, "`path` must be a file path")),
    };
    let config = config_text(p)?;
    let bodies = string_list(p, "bodies");
    let parts = parts(s, &bodies)?;
    let slicer = solidcraft_manufacture::default_slicer();
    let sliced = slicer.slice(&Job { parts: &parts, config: &config }).map_err(|e| EngineError::Other(format!("{SLICE}: {e}")))?;
    if let Some(path) = path {
        solidcraft_io::vfs::write(path, &sliced.gcode).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    }
    let mut out = json!({
        "bytes": sliced.gcode.len(),
        "layers": sliced.layers,
        "segments": sliced.toolpaths.segments(),
        "time_s": sliced.time_s,
        "filament_mm": sliced.filament_mm,
        "filament_g": sliced.filament_g,
        "slicer": slicer.name(),
        "refused": sliced.report.refused.iter().map(|r| json!({"key": r.key, "value": r.value, "reason": r.reason})).collect::<Vec<_>>(),
        "ignored": sliced.report.ignored,
        "warnings": sliced.report.warnings,
    });
    if let (Some(path), Value::Object(map)) = (path, &mut out) {
        map.insert("path".into(), Value::String(path.to_string()));
    }
    s.set_print_slice(Some(HeldSlice { sliced, doc: s.doc.clone(), bodies, config, slicer: slicer.name() }));
    Ok(out)
}

fn held<'a>(s: &'a Session, cmd: &str) -> Result<&'a HeldSlice> {
    s.print_slice().map(|h| &**h).ok_or_else(|| bad(cmd, "nothing sliced yet: run print.slice first"))
}

fn preview(s: &mut Session, p: &Value) -> Result<Value> {
    let held = held(s, PREVIEW)?;
    let paths: &Toolpaths = &held.sliced.toolpaths;
    let bounds = paths.bounds().map(|b| json!({"min": b.min, "max": b.max}));
    let features: Vec<Value> = paths
        .by_feature()
        .iter()
        .map(|t| {
            json!({
                "feature": t.feature.map_or("unknown", |f| f.id()),
                "label": t.feature.map_or("Unknown", |f| f.label()),
                "paths": t.paths,
                "length_mm": t.length_mm,
            })
        })
        .collect();
    let mut out = json!({
        "stale": held.stale(&s.doc),
        "slicer": held.slicer,
        "layers": paths.layers.len(),
        "segments": paths.segments(),
        "time_s": held.sliced.time_s,
        "filament_mm": held.sliced.filament_mm,
        "filament_g": held.sliced.filament_g,
        "bed": held.sliced.bed,
        "printable_height": held.sliced.printable_height,
        "offset": held.sliced.offset,
        "bounds": bounds,
        "z": paths.layers.iter().map(|l| l.z).collect::<Vec<_>>(),
        "features": features,
    });
    match p.get("layer") {
        None | Some(Value::Null) => {}
        Some(v) => {
            let index = v.as_u64().and_then(|i| usize::try_from(i).ok()).ok_or_else(|| bad(PREVIEW, "`layer` must be a layer index"))?;
            let layer = paths.layers.get(index).ok_or_else(|| bad(PREVIEW, format!("no layer {index}: the slice has {}", paths.layers.len())))?;
            let points: usize = layer.paths.iter().map(|p| p.points.len()).sum();
            if points > MAX_PREVIEW_POINTS {
                return Err(bad(
                    PREVIEW,
                    format!(
                        "layer {index} has {points} points, more than the {MAX_PREVIEW_POINTS} one answer carries; read the toolpaths from the session instead"
                    ),
                ));
            }
            let layer = json!({
                "index": index,
                "z": layer.z,
                "height": layer.height,
                "time_s": layer.time_s,
                "paths": layer.paths.iter().map(|p| json!({
                    "feature": p.feature.map_or("unknown", |f| f.id()),
                    "tool": p.tool,
                    "width_mm": p.width_mm,
                    "speed_mm_s": p.speed_mm_s,
                    "points": p.points,
                })).collect::<Vec<_>>(),
            });
            if let Value::Object(map) = &mut out {
                map.insert("layer".into(), layer);
            }
        }
    }
    Ok(out)
}

fn save(s: &mut Session, p: &Value) -> Result<Value> {
    let path = str_(p, "path").filter(|x| !x.trim().is_empty() && x.len() < 4096).ok_or_else(|| bad(SAVE, "`path` is required"))?;
    let held = held(s, SAVE)?;
    solidcraft_io::vfs::write(path, &held.sliced.gcode).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    Ok(json!({"path": path, "bytes": held.sliced.gcode.len(), "stale": held.stale(&s.doc)}))
}

/// The settings as one JSON object text: the files `config` names, which may not define a key
/// twice, with `settings` laid over them.
fn config_text(p: &Value) -> Result<String> {
    let paths: Vec<&str> = match p.get("config") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::String(s)) => vec![s.as_str()],
        Some(Value::Array(list)) => {
            list.iter().map(|v| v.as_str().ok_or_else(|| bad(SLICE, "`config` must be a path or a list of paths"))).collect::<Result<_>>()?
        }
        Some(_) => return Err(bad(SLICE, "`config` must be a path or a list of paths")),
    };
    let mut config: Map<String, Value> = Map::new();
    let mut from: Map<String, Value> = Map::new();
    for path in paths {
        if path.trim().is_empty() || path.len() >= 4096 {
            return Err(bad(SLICE, "`config` paths must not be empty"));
        }
        let bytes = solidcraft_io::vfs::read(path, MAX_CONFIG_BYTES).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
        let map = match serde_json::from_slice::<Value>(&bytes) {
            Ok(Value::Object(map)) => map,
            Ok(_) => return Err(bad(SLICE, format!("{path}: the settings file must hold a JSON object of setting keys"))),
            Err(e) => return Err(bad(SLICE, format!("{path}: {e}"))),
        };
        for (k, v) in map {
            if let Some(Value::String(earlier)) = from.get(&k) {
                return Err(bad(SLICE, format!("setting `{k}` is in both {earlier} and {path}; a key belongs to one file")));
            }
            from.insert(k.clone(), Value::String(path.to_string()));
            config.insert(k, v);
        }
    }
    match p.get("settings") {
        None | Some(Value::Null) => {}
        Some(Value::Object(over)) => config.extend(over.iter().map(|(k, v)| (k.clone(), v.clone()))),
        Some(_) => return Err(bad(SLICE, "`settings` must be an object of setting keys")),
    }
    Ok(Value::Object(config).to_string())
}

/// The bodies to print, tessellated and welded the way the mesh export does, so each is closed
/// where its faces meet.
fn parts(s: &Session, bodies: &[String]) -> Result<Vec<Part>> {
    let state = s.world_state();
    for name in bodies {
        if !state.bodies.iter().any(|b| b.name == *name) {
            return Err(bad(SLICE, format!("unknown body name `{name}`")));
        }
    }
    let mut parts = Vec::new();
    for b in state.bodies.iter().filter(|b| bodies.is_empty() || bodies.contains(&b.name)) {
        let mesh = solidcraft_io::export_mesh(&b.body)?;
        let eps = (b.body.size() * 1e-6).max(1e-7);
        let (positions, triangles) = solidcraft_io::weld(&mesh.positions, &mesh.triangles, eps);
        let positions = positions.iter().map(|p| [p.x as f32, p.y as f32, p.z as f32]).collect();
        parts.push(Part { name: b.name.clone(), positions, triangles });
    }
    if parts.is_empty() {
        return Err(bad(SLICE, "nothing to print: the design has no bodies"));
    }
    Ok(parts)
}
