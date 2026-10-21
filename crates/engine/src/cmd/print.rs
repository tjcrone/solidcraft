//! Slicing the design for 3D printing (`print.*`): the bodies go to the slicer in
//! `solidcraft-manufacture` as welded meshes and come back as G-code. There is no dialog and no
//! toolbar button yet; scripts, the CLI and the control channel drive it.

use serde_json::{Map, Value, json};
use solidcraft_manufacture::{Job, MAX_CONFIG_BYTES, Part, Slicer};

use super::CommandSpec;
use crate::params::{bad, str_, string_list};
use crate::{EngineError, Result, Session};

const SLICE: &str = "print.slice";

pub static COMMANDS: &[CommandSpec] = &[CommandSpec::new(SLICE, "Slice for Printing", slice).noundo().icon("mesh_export").params(
    "path: the .gcode file to write; config?: path, or [paths], of JSON files of OrcaSlicer setting keys (printer, filament and process; no key may appear in two files; keys left out keep the slicer's defaults); settings?: {key: value} laid over the files'; bodies?: [names] (default: all) → path, bytes, layers, time_s, filament_mm, filament_g, slicer, refused: [{key, value, reason}] (settings the slicer could not use and left at their defaults), ignored: [keys the slicer never reads], warnings: [settings lowered to the machine's limits, toolpath findings]; a print that would leave the printer's volume is refused and nothing is written",
)];

fn slice(s: &mut Session, p: &Value) -> Result<Value> {
    let path = str_(p, "path").filter(|x| !x.trim().is_empty() && x.len() < 4096).ok_or_else(|| bad(SLICE, "`path` is required"))?;
    let config = config_text(p)?;
    let parts = parts(s, &string_list(p, "bodies"))?;
    let slicer = solidcraft_manufacture::default_slicer();
    let sliced = slicer.slice(&Job { parts: &parts, config: &config }).map_err(|e| EngineError::Other(format!("{SLICE}: {e}")))?;
    solidcraft_io::vfs::write(path, &sliced.gcode).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    Ok(json!({
        "path": path,
        "bytes": sliced.gcode.len(),
        "layers": sliced.layers,
        "time_s": sliced.time_s,
        "filament_mm": sliced.filament_mm,
        "filament_g": sliced.filament_g,
        "slicer": slicer.name(),
        "refused": sliced.report.refused.iter().map(|r| json!({"key": r.key, "value": r.value, "reason": r.reason})).collect::<Vec<_>>(),
        "ignored": sliced.report.ignored,
        "warnings": sliced.report.warnings,
    }))
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
