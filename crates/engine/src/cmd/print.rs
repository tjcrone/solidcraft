//! Slicing the design for 3D printing (`print.*`): the bodies go to the slicer in
//! `solidcraft-print` as welded meshes and come back as G-code. The session keeps the last
//! slice (`crate::held_slice`): `print.preview` reads its toolpaths for a layer preview and
//! `print.save` writes its G-code, so neither runs the engine again. There is no dialog; the Print
//! workspace, scripts, the CLI and the control channel all run these commands.

use serde_json::{Map, Value, json};
use solidcraft_print::{Job, MAX_CONFIG_BYTES, Part, Slicer, Toolpaths};

use super::CommandSpec;
use crate::held_slice::HeldSlice;
use crate::params::{bad, bool_, string_list};
use crate::printer_host::gcode_file_name;
use crate::schema::print::{StartParams, StartResult};
use crate::schema::{TypedContract, output_schema_for};
use crate::{EngineError, Result, Session};

const START_CONTRACT: TypedContract<StartParams> = TypedContract::new();

const SLICE: &str = "print.slice";
const PREVIEW: &str = "print.preview";
const SAVE: &str = "print.save";
const UPLOAD: &str = "print.upload";
const START: &str = "print.start";
const JOB: &str = "print.job";
const STATUS: &str = "print.status";
const CANCEL: &str = "print.cancel";
const PAUSE: &str = "print.pause";
const RESUME: &str = "print.resume";
const HEAT: &str = "print.heat";
const TIMELAPSE: &str = "print.timelapse";
const WEBCAMS: &str = "print.webcams";
const SETTINGS_GET: &str = "print.settings.get";
const SETTINGS_SET: &str = "print.settings.set";
const SETTINGS_CLEAR: &str = "print.settings.clear";
const PLACE: &str = "print.place";
const BED: &str = "print.bed";
const REFRESH: &str = "print.refresh";
/// Most degrees a nozzle or bed may be asked for.
const MAX_NOZZLE_C: f64 = 350.0;
const MAX_BED_C: f64 = 150.0;

/// Most points one `print.preview` answer carries for a layer, so a reply stays a reply.
const MAX_PREVIEW_POINTS: usize = 2_000_000;

pub static COMMANDS: &[CommandSpec] = &[
    CommandSpec::new(SLICE, "Slice", slice).noundo().at("PREPARE", "PRINT").icon("print_slice").params(
        "check?: true only answers whether the held slice already matches this request → current: bool; path?: a .gcode file to write as well; config?: path, or [paths], of JSON files of OrcaSlicer setting keys (printer, filament and process; no key may appear in two files; keys left out keep the slicer's defaults); settings?: {key: value} laid over the files' and the design's own overrides (print.settings.set); bodies?: [names] (default: all) → path?, bytes, layers, segments, time_s, filament_mm, filament_g, slicer, refused: [{key, value, reason}] (settings the slicer could not use and left at their defaults), ignored: [keys the slicer never reads], warnings: [settings lowered to the machine's limits, toolpath findings]. The session keeps the slice for print.preview and print.save; a print that would leave the printer's volume is refused and nothing is kept or written. A request the held slice already answers (same design, settings and bodies) is not sliced again: the held result comes back with reused: true",
    ),
    CommandSpec::new(PREVIEW, "Preview Toolpaths", preview).noundo().doc_only().icon("inspect").params(
        "layer?: index (0 = first) → stale (the design changed since the slice), slicer, layers, segments, time_s, filament_mm, filament_g, bed: [min_x, min_y, max_x, max_y], printable_height, offset: [x, y, z] (plate coordinates = design coordinates + offset; the toolpaths, bounds and bed are plate coordinates, as the G-code is), bounds: {min: [x, y, z], max: [x, y, z]}, file_beads: {count, length_mm} (beads the start and end templates lay on the bed), z: [top of each layer], features: [{feature, label, paths, length_mm}]; with layer: layer: {index, z, height, time_s, paths: [{feature, tool, width_mm, speed_mm_s, points: [[x, y], …]}]}. Needs a print.slice in this session",
    ),
    CommandSpec::new(SAVE, "Save G-code", save).noundo().doc_only().enabled(has_slice).at("PREVIEW", "OUTPUT").icon("print_save_gcode").params(
        "path: the .gcode file to write with the G-code of the session's last print.slice → path, bytes, stale (the design changed since the slice)",
    ),
    CommandSpec::new(UPLOAD, "Upload to Printer", upload).noundo().doc_only().enabled(can_send).at("PREVIEW", "OUTPUT").icon("print_upload").params(
        "name?: the file name on the printer (default: the design's name; always ends in .gcode); start?: bool (print it at once; default false) → job, name, bytes, start. Sends the session's last print.slice to the installed printer in the background; print.job reports progress. Refused while the design has changed since the slice, and without a printer",
    ),
    CommandSpec::new(START, "Print", crate::checked!("print.start", start))
        .noundo()
        .doc_only()
        .enabled(can_send)
        .at("PREVIEW", "PRINT")
        .icon("print_start")
        .contract(&START_CONTRACT)
        .output(output_schema_for::<StartResult>)
        .params("name?: the file name on the printer (default: the design's name; always ends in .gcode) → job, name, bytes, start: true. Uploads the session's last print.slice to the installed printer and starts printing it (print.upload with start: true); print.job reports progress. Refused while the design has changed since the slice, and without a printer"),
    CommandSpec::new(JOB, "Printer Job", job).noundo().doc_only().icon("list").params(
        "job: id from print.upload or print.cancel → job, state: queued | running (sent, total) | done (message, started) | failed (message)",
    ),
    CommandSpec::new(STATUS, "Printer Status", status).noundo().doc_only().icon("inspect").params(
        "No parameters → installed (a printer is set up), printer: {name, url}, reachable, age_s (since the last good answer), error?, state (standby | printing | paused | complete | cancelled | error), filename, progress (0..1), print_duration_s, extruder: {temperature, target}, bed: {temperature, target}, message, timelapse (bool; null without the printer's timelapse plugin). Never waits on the network: the host's latest answer",
    ),
    CommandSpec::new(CANCEL, "Cancel Print", cancel).noundo().doc_only().at("DEVICE", "JOB").icon("print_cancel").params(
        "No parameters → job. Asks the printer to cancel the running print; print.job reports the answer",
    ),
    CommandSpec::new(PAUSE, "Pause Print", pause).noundo().doc_only().at("DEVICE", "JOB").icon("print_pause").params(
        "No parameters → job. Asks the printer to pause the running print (Klipper's PAUSE); print.job reports the answer",
    ),
    CommandSpec::new(RESUME, "Resume Print", resume).noundo().doc_only().at("DEVICE", "JOB").icon("play").params(
        "No parameters → job. Asks the printer to resume a paused print (Klipper's RESUME); print.job reports the answer",
    ),
    CommandSpec::new(HEAT, "Set Temperatures", heat).noundo().doc_only().icon("settings").params(
        "nozzle?: °C (0 turns the heater off, at most 350); bed?: °C (at most 150); at least one → jobs: [ids]. Sends M104 and M140 to the printer; print.job reports each answer",
    ),
    CommandSpec::new(TIMELAPSE, "Printer Timelapse", timelapse).noundo().doc_only().icon("camera").params(
        "enabled: bool → job. Turns the printer's timelapse (Moonraker's timelapse plugin) on or off at once, during a print too; print.status reports it as timelapse; print.job reports the answer",
    ),
    CommandSpec::new(WEBCAMS, "Printer Cameras", webcams).noundo().doc_only().icon("list").params(
        "No parameters → webcams: [{name, stream_url, snapshot_url, enabled, rotation, flip_horizontal, flip_vertical}], as the printer lists them",
    ),
    CommandSpec::new(SETTINGS_GET, "Read Print Settings", settings_get).noundo().doc_only().icon("settings").params(
        "config?: path, or [paths], of the settings files; keys?: [names] (default: the design's overrides and every key of the files) → settings: {key: {value, source: design | file | default, file?}}, overlay: {key: value} (the design's own overrides)",
    ),
    CommandSpec::new(SETTINGS_SET, "3D Print Settings", settings_set).doc_only().icon("settings").params(
        "set: {key: value} (OrcaSlicer key names; a null value removes the override) → overlay: {key: value}. Saved in the design; print.slice lays them over the settings files",
    ),
    CommandSpec::new(SETTINGS_CLEAR, "Clear Print Settings", settings_clear).doc_only().icon("delete").params("keys?: [names] (default: all) → overlay: {key: value}"),
    CommandSpec::new(PLACE, "Centre on Bed", place).doc_only().at("PREPARE", "PLACE").icon("print_centre_on_bed").params(
        "x?: mm from the bed's centre; y?: mm; rotation?: degrees about Z, about the design's footprint centre; centre?: true puts it back in the middle, unturned → placement: {x, y, rotation}. Saved in the design; print.slice puts the design there",
    ),
    CommandSpec::new(BED, "Print Bed", bed).noundo().doc_only().icon("inspect").params(
        "config?: path, or [paths], of the settings files → bed: [min_x, min_y, max_x, max_y], printable_height, placement: {x, y, rotation}. What the settings describe, without slicing",
    ),
    CommandSpec::new(REFRESH, "Refresh Printer", refresh).noundo().doc_only().icon("inspect").params(
        "No parameters. Asks the installed printer host for its status and cameras now, without waiting for its next poll; print.status and print.webcams show what came back",
    ),
];

fn refresh(s: &mut Session, _p: &Value) -> Result<Value> {
    host(s, REFRESH)?.refresh();
    Ok(json!({}))
}

fn place(s: &mut Session, p: &Value) -> Result<Value> {
    let number = |key: &str| -> Result<Option<f64>> {
        match p.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => v.as_f64().filter(|x| x.is_finite()).map(Some).ok_or_else(|| bad(PLACE, format!("`{key}` must be a finite number"))),
        }
    };
    let mut settings = (*s.doc.print).clone();
    if p.get("centre").and_then(Value::as_bool) == Some(true) || p.get("center").and_then(Value::as_bool) == Some(true) {
        settings.placement = Default::default();
    }
    if let Some(x) = number("x")? {
        settings.placement.x = x;
    }
    if let Some(y) = number("y")? {
        settings.placement.y = y;
    }
    if let Some(r) = number("rotation")? {
        settings.placement.rotation = r;
    }
    settings.validate().map_err(|e| bad(PLACE, e.to_string()))?;
    if settings != *s.doc.print {
        s.doc_mut().print = std::sync::Arc::new(settings);
    }
    Ok(json!({"placement": s.doc.print.placement}))
}

fn bed(s: &mut Session, p: &Value) -> Result<Value> {
    let config = config_text(s, p)?;
    let (bed, printable_height) = solidcraft_print::bed_of(&config).map_err(|e| EngineError::Other(format!("{BED}: {e}")))?;
    Ok(json!({"bed": bed, "printable_height": printable_height, "placement": s.doc.print.placement}))
}

fn settings_get(s: &mut Session, p: &Value) -> Result<Value> {
    let (files, from) = config_files(p, SETTINGS_GET)?;
    let overlay = &s.doc.print.overlay;
    let keys: Vec<String> = match p.get("keys") {
        None | Some(Value::Null) => files.keys().chain(overlay.keys()).cloned().collect::<std::collections::BTreeSet<_>>().into_iter().collect(),
        Some(Value::Array(list)) => {
            list.iter().map(|k| k.as_str().map(str::to_string).ok_or_else(|| bad(SETTINGS_GET, "`keys` must be names"))).collect::<Result<_>>()?
        }
        Some(_) => return Err(bad(SETTINGS_GET, "`keys` must be a list of names")),
    };
    if keys.len() > 2000 {
        return Err(bad(SETTINGS_GET, "too many keys"));
    }
    let mut settings = Map::new();
    for key in keys {
        let entry = if let Some(v) = overlay.get(&key) {
            json!({"value": v, "source": "design"})
        } else if let Some(v) = files.get(&key) {
            json!({"value": v, "source": "file", "file": from.get(&key)})
        } else {
            json!({"value": Value::Null, "source": "default"})
        };
        settings.insert(key, entry);
    }
    Ok(json!({"settings": settings, "overlay": overlay}))
}

fn settings_set(s: &mut Session, p: &Value) -> Result<Value> {
    use solidcraft_doc::print_settings::PrintSettings;
    let set = p.get("set").and_then(Value::as_object).ok_or_else(|| bad(SETTINGS_SET, "`set` must be an object of settings"))?;
    if set.is_empty() {
        return Err(bad(SETTINGS_SET, "`set` is empty"));
    }
    let mut settings = (*s.doc.print).clone();
    for (key, value) in set {
        PrintSettings::validate_key(key).map_err(|e| bad(SETTINGS_SET, e.to_string()))?;
        if value.is_null() {
            settings.overlay.remove(key);
        } else {
            PrintSettings::validate_value(key, value).map_err(|e| bad(SETTINGS_SET, e.to_string()))?;
            settings.overlay.insert(key.clone(), value.clone());
        }
    }
    settings.validate().map_err(|e| bad(SETTINGS_SET, e.to_string()))?;
    if settings != *s.doc.print {
        s.doc_mut().print = std::sync::Arc::new(settings);
    }
    Ok(json!({"overlay": s.doc.print.overlay}))
}

fn settings_clear(s: &mut Session, p: &Value) -> Result<Value> {
    let keys = string_list(SETTINGS_CLEAR, p, "keys")?;
    let mut settings = (*s.doc.print).clone();
    if keys.is_empty() {
        settings.overlay.clear();
    } else {
        for key in &keys {
            settings.overlay.remove(key);
        }
    }
    if settings != *s.doc.print {
        s.doc_mut().print = std::sync::Arc::new(settings);
    }
    Ok(json!({"overlay": s.doc.print.overlay}))
}

fn pause(s: &mut Session, _p: &Value) -> Result<Value> {
    let job = host(s, PAUSE)?.script("PAUSE").map_err(|e| EngineError::Other(format!("{PAUSE}: {e}")))?;
    Ok(json!({"job": job}))
}

fn resume(s: &mut Session, _p: &Value) -> Result<Value> {
    let job = host(s, RESUME)?.script("RESUME").map_err(|e| EngineError::Other(format!("{RESUME}: {e}")))?;
    Ok(json!({"job": job}))
}

fn heat(s: &mut Session, p: &Value) -> Result<Value> {
    let degrees = |key: &str, most: f64| -> Result<Option<f64>> {
        match p.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => {
                let t = v.as_f64().filter(|t| t.is_finite()).ok_or_else(|| bad(HEAT, format!("`{key}` must be a number of degrees")))?;
                if !(0.0..=most).contains(&t) {
                    return Err(bad(HEAT, format!("`{key}` must be between 0 and {most} °C")));
                }
                Ok(Some(t))
            }
        }
    };
    let nozzle = degrees("nozzle", MAX_NOZZLE_C)?;
    let bed = degrees("bed", MAX_BED_C)?;
    if nozzle.is_none() && bed.is_none() {
        return Err(bad(HEAT, "give `nozzle` and/or `bed` in °C"));
    }
    let host = host(s, HEAT)?;
    let mut jobs = Vec::new();
    if let Some(t) = nozzle {
        jobs.push(host.script(&format!("M104 S{t:.0}")).map_err(|e| EngineError::Other(format!("{HEAT}: {e}")))?);
    }
    if let Some(t) = bed {
        jobs.push(host.script(&format!("M140 S{t:.0}")).map_err(|e| EngineError::Other(format!("{HEAT}: {e}")))?);
    }
    Ok(json!({"jobs": jobs}))
}

fn timelapse(s: &mut Session, p: &Value) -> Result<Value> {
    let enabled = bool_(p, "enabled").ok_or_else(|| bad(TIMELAPSE, "give `enabled`: true or false"))?;
    let job = host(s, TIMELAPSE)?.set_timelapse(enabled).map_err(|e| EngineError::Other(format!("{TIMELAPSE}: {e}")))?;
    Ok(json!({"job": job}))
}

fn webcams(s: &mut Session, _p: &Value) -> Result<Value> {
    let list = host(s, WEBCAMS)?.webcams();
    Ok(json!({"webcams": list}))
}

fn host<'a>(s: &'a Session, cmd: &str) -> Result<&'a std::sync::Arc<dyn crate::printer_host::PrinterHost>> {
    s.printer_host().ok_or_else(|| bad(cmd, "no printer is set up"))
}

fn upload(s: &mut Session, p: &Value) -> Result<Value> {
    let start = bool_(p, "start").unwrap_or(false);
    let (job, name, bytes) = send(s, p, start, UPLOAD)?;
    Ok(json!({"job": job, "name": name, "bytes": bytes, "start": start}))
}

/// Save G-code: a slice is held.
fn has_slice(s: &Session) -> std::result::Result<(), String> {
    if s.print_slice().is_some() { Ok(()) } else { Err("nothing is sliced yet: slice the design first".into()) }
}

/// Upload and Print: a slice of the design as it is now, and a printer to send it to.
fn can_send(s: &Session) -> std::result::Result<(), String> {
    match s.print_slice() {
        None => return Err("nothing is sliced yet: slice the design first".into()),
        Some(held) if held.stale(&s.doc) => return Err("the design changed since the last slice: slice it again".into()),
        Some(_) => {}
    }
    if s.printer_host().is_none() {
        return Err("no printer is set up: add its Moonraker address in the Device tab".into());
    }
    Ok(())
}

/// Print: the upload with `start` always true, its reply built from [`StartResult`].
fn start(s: &mut Session, p: &Value) -> Result<Value> {
    let (job, name, bytes) = send(s, p, true, START)?;
    serde_json::to_value(StartResult { job, name, bytes: bytes as u64, start: true, param_warnings: Vec::new() })
        .map_err(|e| EngineError::Other(format!("{START}: {e}")))
}

/// Sends the held slice's G-code to the printer under `name` (default: the design's name):
/// the upload job, the file name and its size in bytes.
fn send(s: &mut Session, p: &Value, start: bool, cmd: &str) -> Result<(u64, String, usize)> {
    let held = held(s, cmd)?;
    if held.stale(&s.doc) {
        return Err(bad(cmd, "the design changed since the last slice; slice it again before uploading"));
    }
    let name = match p.get("name") {
        None | Some(Value::Null) => gcode_file_name(&s.doc.name),
        Some(Value::String(n)) => gcode_file_name(n),
        Some(_) => return Err(bad(cmd, "`name` must be a file name")),
    };
    let bytes = held.sliced.gcode.len();
    let gcode = held.sliced.gcode.clone();
    let job = host(s, cmd)?.upload(&name, gcode, start).map_err(|e| EngineError::Other(format!("{cmd}: {e}")))?;
    Ok((job, name, bytes))
}

fn job(s: &mut Session, p: &Value) -> Result<Value> {
    let id = p.get("job").and_then(Value::as_u64).ok_or_else(|| bad(JOB, "`job` is required"))?;
    let state = host(s, JOB)?.job(id).ok_or_else(|| bad(JOB, format!("no job {id}")))?;
    let mut out = serde_json::to_value(&state).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut out {
        map.insert("job".into(), json!(id));
        map.insert("finished".into(), json!(state.finished()));
    }
    Ok(out)
}

fn status(s: &mut Session, _p: &Value) -> Result<Value> {
    let Some(host) = s.printer_host() else {
        return Ok(json!({"installed": false, "reachable": false, "state": "", "error": "no printer is set up"}));
    };
    let mut out = serde_json::to_value(host.status()).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut out {
        map.insert("installed".into(), json!(true));
        map.insert("printer".into(), serde_json::to_value(host.info()).unwrap_or(Value::Null));
    }
    Ok(out)
}

fn cancel(s: &mut Session, _p: &Value) -> Result<Value> {
    let job = host(s, CANCEL)?.cancel_print().map_err(|e| EngineError::Other(format!("{CANCEL}: {e}")))?;
    Ok(json!({"job": job}))
}

/// The file a command writes its G-code to: an absolute path, so a relative one (a script's
/// mistake, a hostile value) never lands in whatever folder the process happens to run in.
fn output_path<'a>(p: &'a Value, cmd: &str) -> Result<Option<&'a str>> {
    match p.get("path") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(x)) if !x.trim().is_empty() && x.len() < 4096 => {
            if std::path::Path::new(x.trim()).is_absolute() {
                Ok(Some(x.as_str()))
            } else {
                Err(bad(cmd, "`path` must be an absolute file path"))
            }
        }
        Some(_) => Err(bad(cmd, "`path` must be a file path")),
    }
}

fn slice(s: &mut Session, p: &Value) -> Result<Value> {
    let path = output_path(p, SLICE)?;
    let config = config_text(s, p)?;
    let bodies = string_list(SLICE, p, "bodies")?;
    // The held slice already answers this request: the same design (placement and overrides
    // included), the same settings text (a profile file edited on disk reads differently) and
    // the same bodies.
    let current = s.print_slice().filter(|h| !h.stale(&s.doc) && h.config == config && h.bodies == bodies).cloned();
    if bool_(p, "check").unwrap_or(false) {
        return Ok(json!({"current": current.is_some()}));
    }
    if let Some(held) = current {
        if let Some(path) = path {
            solidcraft_io::vfs::write(path, &held.sliced.gcode).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
        }
        let mut out = slice_result(&held.sliced, &held.slicer, path);
        if let Value::Object(map) = &mut out {
            map.insert("reused".into(), Value::Bool(true));
        }
        return Ok(out);
    }
    let parts = parts(s, &bodies)?;
    let slicer = solidcraft_print::default_slicer();
    let pl = s.doc.print.placement;
    let placement = solidcraft_print::Placement { x: pl.x as f32, y: pl.y as f32, rotation_deg: pl.rotation as f32 };
    let sliced = slicer.slice(&Job { parts: &parts, config: &config, placement }).map_err(|e| EngineError::Other(format!("{SLICE}: {e}")))?;
    if let Some(path) = path {
        solidcraft_io::vfs::write(path, &sliced.gcode).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    }
    let out = slice_result(&sliced, &slicer.name(), path);
    s.set_print_slice(Some(HeldSlice { sliced, doc: s.doc.clone(), bodies, config, slicer: slicer.name() }));
    Ok(out)
}

/// What print.slice reports about a slice.
fn slice_result(sliced: &solidcraft_print::Sliced, slicer: &str, path: Option<&str>) -> Value {
    let mut out = json!({
        "bytes": sliced.gcode.len(),
        "layers": sliced.layers,
        "segments": sliced.toolpaths.segments(),
        "time_s": sliced.time_s,
        "filament_mm": sliced.filament_mm,
        "filament_g": sliced.filament_g,
        "slicer": slicer,
        "refused": sliced.report.refused.iter().map(|r| json!({"key": r.key, "value": r.value, "reason": r.reason})).collect::<Vec<_>>(),
        "ignored": sliced.report.ignored,
        "warnings": sliced.report.warnings,
    });
    if let (Some(path), Value::Object(map)) = (path, &mut out) {
        map.insert("path".into(), Value::String(path.to_string()));
    }
    out
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
        "placement": held.doc.print.placement,
        "file_beads": {"count": paths.file_beads.len(), "length_mm": paths.file_beads.iter().map(|b| b.length_mm()).sum::<f64>()},
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
    let path = output_path(p, SAVE)?.ok_or_else(|| bad(SAVE, "`path` is required"))?;
    let held = held(s, SAVE)?;
    solidcraft_io::vfs::write(path, &held.sliced.gcode).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    Ok(json!({"path": path, "bytes": held.sliced.gcode.len(), "stale": held.stale(&s.doc)}))
}

/// The settings files `config` names, read as one map that may not define a key twice, and
/// which file each key came from.
fn config_files(p: &Value, cmd: &str) -> Result<(Map<String, Value>, Map<String, Value>)> {
    let paths: Vec<&str> = match p.get("config") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::String(s)) => vec![s.as_str()],
        Some(Value::Array(list)) => {
            list.iter().map(|v| v.as_str().ok_or_else(|| bad(cmd, "`config` must be a path or a list of paths"))).collect::<Result<_>>()?
        }
        Some(_) => return Err(bad(cmd, "`config` must be a path or a list of paths")),
    };
    let mut config: Map<String, Value> = Map::new();
    let mut from: Map<String, Value> = Map::new();
    for path in paths {
        if path.trim().is_empty() || path.len() >= 4096 {
            return Err(bad(cmd, "`config` paths must not be empty"));
        }
        let bytes = solidcraft_io::vfs::read(path, MAX_CONFIG_BYTES).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
        let map = match serde_json::from_slice::<Value>(&bytes) {
            Ok(Value::Object(map)) => map,
            Ok(_) => return Err(bad(cmd, format!("{path}: the settings file must hold a JSON object of setting keys"))),
            Err(e) => return Err(bad(cmd, format!("{path}: {e}"))),
        };
        for (k, v) in map {
            if let Some(Value::String(earlier)) = from.get(&k) {
                return Err(bad(cmd, format!("setting `{k}` is in both {earlier} and {path}; a key belongs to one file")));
            }
            from.insert(k.clone(), Value::String(path.to_string()));
            config.insert(k, v);
        }
    }
    Ok((config, from))
}

/// The settings as one JSON object text: the files `config` names, the design's own overrides
/// over them, and `settings` over both.
fn config_text(s: &Session, p: &Value) -> Result<String> {
    let (mut config, _) = config_files(p, SLICE)?;
    config.extend(s.doc.print.overlay.iter().map(|(k, v)| (k.clone(), v.clone())));
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
