//! The Print workspace: slice the design for 3D printing, look at the toolpaths layer by layer
//! and save the G-code. Everything it does goes through the `print.*` commands; the canvas and
//! the panel read the slice the session holds (`solidcraft_engine::held_slice`), so the CLI and
//! the control channel see the same state.

use std::sync::{Arc, Mutex};

use egui::{Color32, Sense, vec2};
use serde::{Deserialize, Serialize};
use serde_json::json;
use solidcraft_engine::geom::{Aabb3, Vec3};
use solidcraft_engine::held_slice::HeldSlice;
use solidcraft_engine::print::{Feature, Layer, Toolpaths};
use solidcraft_engine::printer_host::PrinterHost;

use crate::gpu::{Frame, GpuScene, PaletteIndex, SceneSlot, ViewportCallback};
use crate::ui_kit::{Container, Edge, Surface};
use crate::{SolveApp, theme::Tokens};

/// Bytes of toolpath geometry one preview may upload. Past it the ribbons lose their sides, then
/// become lines, so a big print still shows.
const MAX_GEOMETRY_BYTES: usize = 128 * 1024 * 1024;
const TRI_BYTES: usize = 3 * crate::gpu::TRI_SIZE;
const LINE_BYTES: usize = crate::gpu::LINE_SIZE;

/// What the workspace keeps between runs: the settings files the slicer reads.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// OrcaSlicer-style JSON files (printer, filament, process), in the order given.
    pub config_files: Vec<String>,
    /// The printer's Moonraker address (`http://printer.local`); empty for no printer.
    pub printer_url: String,
}

/// Makes a printer host for a Moonraker address; the desktop app installs it in `Services`.
pub type MakePrinterHost = Arc<dyn Fn(&str) -> Result<Arc<dyn PrinterHost>, String> + Send + Sync>;

pub struct State {
    pub settings: Settings,
    settings_loaded: bool,
    /// The offset the camera target currently carries (`sync_view`).
    view_offset: [f32; 3],
    /// The offset the view took on entering the workspace, so the Design view and this one
    /// show the part in the same place at that moment; it stays until the workspace is left,
    /// so moving the part never moves the camera.
    anchor: Option<[f32; 3]>,
    /// The highest layer shown, as an index; `None` shows every layer.
    pub top_layer: Option<usize>,
    pub show_travels: bool,
    /// The address a host was last made for, so a bad address is reported once.
    printer_tried: Option<String>,
    /// The printer job the panels follow: an upload, a cancel, a pause, a resume or a heat.
    pub job: Option<Job>,
    /// The "start printing?" question is open.
    confirm_print: bool,
    slot: SceneSlot,
    key: Option<u64>,
    /// What the uploaded scene was made from.
    uploaded: Option<Source>,
    /// The bed the settings describe, read once per change of the files or the design.
    bed: Option<BedInfo>,
    /// A drag of the part across the bed in Prepare.
    drag: Option<Drag>,
    /// Whether the held slice answers the panel's request, and what that was checked for.
    current: Option<(SliceKey, bool)>,
    /// The bead detail the preview's toolpaths were last drawn at.
    detail: Option<Detail>,
    /// The toolpaths of every layer, and the slice and travel setting they were built for. The
    /// slice itself is kept, so a new one can never take its address and pass for it.
    layered: Option<(Arc<HeldSlice>, bool, Arc<Layered>)>,
}

/// What a slice request depends on: the files and when they changed, the design, the slice.
#[derive(Clone, PartialEq, Eq)]
struct SliceKey {
    files: Vec<(String, Option<u64>)>,
    revision: u64,
    held: usize,
}

/// The bed `print.bed` reported, and what it was read for.
struct BedInfo {
    key: (Vec<String>, u64),
    rect: [f32; 4],
}

/// The part being dragged across the bed: where on the bed the drag began, the placement it
/// began from, and where the pointer is now.
#[derive(Clone, Copy, Debug)]
struct Drag {
    from: [f64; 2],
    start: [f64; 2],
    to: [f64; 2],
}

/// A request the printer host runs in the background, and what it is, so the panels can say
/// where it stands: Klipper queues a cancel or a pause behind the macro it is running (a start
/// sequence heating for minutes), and the printer's own state says when it took effect.
#[derive(Clone, Copy, Debug)]
pub struct Job {
    pub id: u64,
    pub kind: JobKind,
    pub since: std::time::Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobKind {
    Upload,
    Cancel,
    Pause,
    Resume,
    Heat,
    Timelapse,
}

impl Job {
    pub fn new(id: u64, kind: JobKind) -> Self {
        Self { id, kind, since: std::time::Instant::now() }
    }
}

/// What the panels say about a job: the message id, and whether it is a warning. `job_state`
/// is what print.job reported (`queued`, `running`, `done`, `failed`), `printer_state` what the
/// printer reports now, `waited` how long since the request went out. A cancel or a pause is
/// judged by the printer's state, not the request's answer: Moonraker holds the request while
/// Klipper finishes its macro and may answer late or with an error once the print is gone.
pub fn job_text(kind: JobKind, job_state: &str, printer_state: &str, waited: std::time::Duration) -> (&'static str, bool) {
    let pending = matches!(job_state, "queued" | "running");
    let queued = pending && waited.as_secs() >= 2;
    match kind {
        JobKind::Cancel if matches!(printer_state, "cancelled" | "standby" | "complete" | "error") => ("print.cancel_done", false),
        JobKind::Cancel if queued => ("print.cancel_queued", false),
        JobKind::Pause if printer_state == "paused" => ("print.pause_done", false),
        JobKind::Pause if queued => ("print.pause_queued", false),
        JobKind::Resume if printer_state == "printing" && !pending => ("print.resume_done", false),
        JobKind::Resume if queued => ("print.resume_queued", false),
        _ if pending => ("print.job_running", false),
        _ if job_state == "failed" => ("", true),
        _ => ("", false),
    }
}

/// The line about the job the panels follow, under the printer's state. Clears a job the host
/// no longer knows.
pub fn job_line(app: &mut SolveApp, ui: &mut egui::Ui, printer_state: &str) {
    let Some(job) = app.print_workspace.job else { return };
    let t = Tokens::get();
    let reply = match app.run("print.job", json!({ "job": job.id })) {
        Ok(j) => j,
        Err(_) => {
            app.print_workspace.job = None;
            return;
        }
    };
    let job_state = reply["state"].as_str().unwrap_or("").to_string();
    let message = reply["message"].as_str().unwrap_or("").to_string();
    let (id, warning) = job_text(job.kind, &job_state, printer_state, job.since.elapsed());
    let text = if id.is_empty() {
        if job.kind == JobKind::Upload && job_state == "done" && reply["started"] == true {
            crate::i18n::format("print.job_started", &[("message", &message)])
        } else {
            message
        }
    } else {
        app.tr(id).to_string()
    };
    if warning {
        ui.colored_label(t.warning, text);
    } else if !text.is_empty() {
        ui.small(text);
    }
    if matches!(job_state.as_str(), "queued" | "running") {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(300));
    }
}

/// What the canvas shows: the held slice's toolpaths, or the design's bodies before any slice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    Toolpaths {
        slice: usize,
        travels: bool,
        /// How many layers, from the first, are shown.
        shown: usize,
    },
    /// The design placed on the bed: the placement in thousandths of a millimetre and of a
    /// degree, the bed in tenths of a millimetre.
    Prepared {
        revision: u64,
        world: usize,
        placement: [i64; 3],
        bed: [i32; 4],
        /// A drag in progress, tenths of a millimetre.
        drag: [i64; 2],
    },
}

impl Default for State {
    fn default() -> Self {
        Self {
            settings: Settings::default(),
            settings_loaded: false,
            view_offset: [0.0; 3],
            anchor: None,
            top_layer: None,
            show_travels: false,
            printer_tried: None,
            job: None,
            confirm_print: false,
            slot: Arc::new(Mutex::new(None)),
            key: None,
            uploaded: None,
            bed: None,
            drag: None,
            current: None,
            detail: None,
            layered: None,
        }
    }
}

/// The Print workspace is showing: its Print tab or its Printer tab.
/// The Print workspace's tabs: the model on the bed, the sliced preview, the printer.
pub const PREPARE: &str = "PREPARE";
pub const PREVIEW: &str = "PREVIEW";
pub const DEVICE: &str = "DEVICE";

pub fn is_tab(tab: &str) -> bool {
    matches!(tab, PREPARE | PREVIEW | DEVICE)
}

pub fn active(app: &SolveApp) -> bool {
    is_tab(&app.ui.tab)
}

/// The colour a feature's beads are drawn in (straight sRGB). Built in OKLCH with the lightness
/// near 0.7 and the chroma near 0.1 for every feature, so the walls, surfaces and infill tell
/// apart by hue without any of them shouting; greys for support, near-white for gap fill.
pub fn feature_color(feature: Option<Feature>) -> [u8; 4] {
    match feature {
        Some(Feature::OuterWall) => [0xd5, 0x5c, 0x13, 255],
        Some(Feature::InnerWall) => [0xd5, 0xb4, 0x55, 255],
        Some(Feature::OverhangWall) => [0x5a, 0x86, 0xce, 255],
        Some(Feature::TopSurface) => [0xd5, 0x6e, 0x78, 255],
        Some(Feature::BottomSurface) => [0x9f, 0x71, 0xb4, 255],
        Some(Feature::InternalSolid) => [0x9a, 0x50, 0x4b, 255],
        Some(Feature::SparseInfill) => [0x72, 0x00, 0x13, 255],
        Some(Feature::Bridge) => [0x4d, 0xb4, 0xcc, 255],
        Some(Feature::InternalBridge) => [0x34, 0x89, 0x8d, 255],
        Some(Feature::Support) => [0xab, 0xab, 0xab, 255],
        Some(Feature::SupportInterface) => [0x7a, 0x7a, 0x7a, 255],
        Some(Feature::Brim) => [0xcf, 0xbb, 0x9a, 255],
        Some(Feature::Skirt) => [0xa3, 0xc8, 0xb3, 255],
        Some(Feature::Ironing) => [0xd7, 0x93, 0xb3, 255],
        Some(Feature::GapFill) => [0xd7, 0xd7, 0xd7, 255],
        Some(Feature::PrimeTower) => [0x9f, 0x9f, 0xcf, 255],
        Some(Feature::Custom) | None => [0x97, 0x9e, 0x6c, 255],
    }
}

const TRAVEL_COLOR: [u8; 4] = [0x6a, 0x8f, 0xd0, 160];

/// The colour of the beads the start and end templates lay (the purge line).
const FILE_BEAD_COLOR: [u8; 4] = [0x3a, 0x6e, 0xe8, 255];

/// Enter (`on`) or leave the workspace. The view does not jump: Print shows the part where the
/// slicer put it on the bed, and the camera target moves by that placement with it (`sync_view`).
pub fn enter(app: &mut SolveApp, on: bool) -> Result<(), String> {
    if on {
        if app.session.active_sketch.is_some() || app.tool.is_some() || app.dialog.is_some() || app.file_load.busy() {
            return Err("finish the current modelling operation before entering Print".into());
        }
        if crate::render_workspace::active(app) {
            crate::render_workspace::enter(app, false)?;
        }
        app.ui.tab = PREPARE.into();
    } else if active(app) {
        app.ui.tab = "MODEL".into();
    }
    app.cancel_view_animation();
    // The anchor is taken on the first frame drawn, once the bed is known; leaving takes it off.
    app.print_workspace.anchor = None;
    if !on {
        sync_view(app);
    }
    crate::printer_dashboard::sync_camera(app);
    Ok(())
}

/// The placement the view should carry now: the held slice's offset while Print shows it, else
/// none. The camera target follows any change, so the part stays where it was on screen when
/// the workspace changes or a new slice lands elsewhere on the bed.
pub fn sync_view(app: &mut SolveApp) {
    let wanted = if active(app) {
        match app.print_workspace.anchor {
            Some(a) => a,
            None => {
                let a = placed_offset(app);
                app.print_workspace.anchor = Some(a);
                a
            }
        }
    } else {
        app.print_workspace.anchor = None;
        [0.0; 3]
    };
    let applied = app.print_workspace.view_offset;
    if wanted != applied {
        let d = view_shift(applied, wanted);
        app.cam.target += d;
        app.print_workspace.view_offset = wanted;
    }
}

/// What the camera target moves by when the carried placement goes from `applied` to `wanted`.
fn view_shift(applied: [f32; 3], wanted: [f32; 3]) -> Vec3 {
    Vec3::new(f64::from(wanted[0] - applied[0]), f64::from(wanted[1] - applied[1]), f64::from(wanted[2] - applied[2]))
}

/// Up and Down step through the layers (Shift: ten at a time) while a slice is shown; the other
/// view keys keep their meaning. Returns whether the key was taken.
pub fn layer_key(app: &mut SolveApp, key: egui::Key, mods: egui::Modifiers) -> bool {
    if !active(app) || mods.command || mods.ctrl || mods.alt {
        return false;
    }
    if app.ui.tab == PREPARE {
        return nudge_key(app, key, mods);
    }
    let step: i64 = match key {
        egui::Key::ArrowUp => 1,
        egui::Key::ArrowDown => -1,
        _ => return false,
    } * if mods.shift { 10 } else { 1 };
    let Some(held) = app.session.print_slice() else { return false };
    let n = held.sliced.toolpaths.layers.len();
    if n == 0 {
        return false;
    }
    let shown = (shown_layer(app, n) as i64 + step).clamp(1, n as i64) as usize;
    app.print_workspace.top_layer = (shown < n).then_some(shown - 1);
    true
}

/// The workspace menu's choice: `design`, `render` or `print`.
/// The arrow keys on Prepare move the part a millimetre on the bed, ten with Shift.
fn nudge_key(app: &mut SolveApp, key: egui::Key, mods: egui::Modifiers) -> bool {
    let (dx, dy): (f64, f64) = match key {
        egui::Key::ArrowUp => (0.0, 1.0),
        egui::Key::ArrowDown => (0.0, -1.0),
        egui::Key::ArrowLeft => (-1.0, 0.0),
        egui::Key::ArrowRight => (1.0, 0.0),
        _ => return false,
    };
    let step = if mods.shift { 10.0 } else { 1.0 };
    let p = app.session.doc.print.placement;
    place(app, json!({"x": p.x + dx * step, "y": p.y + dy * step}));
    true
}

/// Whether the held slice already answers what Slice would ask for: then Slice is greyed. The
/// engine is asked once per change of the files, their modification times, the design or the
/// held slice.
fn slice_current(app: &mut SolveApp) -> bool {
    let files = app.print_workspace.settings.config_files.clone();
    let stamps: Vec<(String, Option<u64>)> = files
        .iter()
        .map(|f| {
            let modified =
                std::fs::metadata(f).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs());
            (f.clone(), modified)
        })
        .collect();
    let key = SliceKey { files: stamps, revision: app.session.revision, held: app.session.print_slice().map_or(0, |h| Arc::as_ptr(h) as usize) };
    if let Some((k, v)) = &app.print_workspace.current
        && *k == key
    {
        return *v;
    }
    let current =
        app.session.print_slice().is_some() && app.run("print.slice", json!({"check": true, "config": files})).is_ok_and(|v| v["current"] == true);
    app.print_workspace.current = Some((key, current));
    current
}

/// The Slice button, greyed while the held slice is up to date.
fn slice_button(app: &mut SolveApp, ui: &mut egui::Ui) {
    let current = slice_current(app);
    let button = crate::ui_kit::button(
        ui,
        app.tr("cmd.print.slice"),
        if current { crate::ui_kit::Button::Enabled(false) } else { crate::ui_kit::Button::Primary },
    );
    let button = if current { button.on_disabled_hover_text(app.tr("print.slice_current")) } else { button };
    if button.clicked() {
        slice(app);
    }
}

/// Run `print.place`, reporting a refusal on the status line.
fn place(app: &mut SolveApp, params: serde_json::Value) {
    if let Err(e) = app.run("print.place", params) {
        app.set_status(e, true);
    }
}

/// Where the part stands in printer coordinates: the slice's own offset on the Slice tab, else
/// the placement laid over the bed the settings describe.
fn placed_offset(app: &SolveApp) -> [f32; 3] {
    if app.ui.tab == PREVIEW
        && let Some(held) = app.session.print_slice()
    {
        return held.sliced.offset;
    }
    prepare_offset(app).unwrap_or([0.0; 3])
}

/// The design's offset on the bed before any slice: its footprint's centre goes to the bed's
/// centre plus the placement, and it sits on the bed.
fn prepare_offset(app: &SolveApp) -> Option<[f32; 3]> {
    let bed = app.print_workspace.bed.as_ref()?.rect;
    let b = solidcraft_engine::product_scene::extract(&app.session).ok()?.bounds;
    if b.is_empty() {
        return None;
    }
    let p = app.session.doc.print.placement;
    let c = b.center();
    Some([f32::midpoint(bed[0], bed[2]) + p.x as f32 - c.x as f32, f32::midpoint(bed[1], bed[3]) + p.y as f32 - c.y as f32, -(b.min.z as f32)])
}

/// Read the bed from the settings when the files or the design changed.
fn bed_info(app: &mut SolveApp) {
    let key = (app.print_workspace.settings.config_files.clone(), app.session.revision);
    if app.print_workspace.bed.as_ref().is_some_and(|b| b.key == key) {
        return;
    }
    let rect = app
        .run("print.bed", json!({"config": key.0}))
        .ok()
        .and_then(|v| serde_json::from_value::<[f32; 4]>(v["bed"].clone()).ok())
        .filter(|r| r.iter().all(|v| v.is_finite()) && r[2] > r[0] && r[3] > r[1]);
    app.print_workspace.bed = rect.map(|rect| BedInfo { key, rect });
}

pub fn switch(app: &mut SolveApp, workspace: &str) -> Result<(), String> {
    match workspace {
        "print" => enter(app, true),
        "render" => {
            if active(app) {
                enter(app, false)?;
            }
            crate::render_workspace::enter(app, true)
        }
        _ => {
            if active(app) {
                enter(app, false)
            } else {
                crate::render_workspace::enter(app, false)
            }
        }
    }
}

/// Another design opened: the view starts over, the settings files stay.
pub fn reset_document(app: &mut SolveApp) {
    app.print_workspace.view_offset = [0.0; 3];
    app.print_workspace.anchor = None;
    let settings = std::mem::take(&mut app.print_workspace.settings);
    let settings_loaded = app.print_workspace.settings_loaded;
    app.print_workspace = State { settings, settings_loaded, ..Default::default() };
    if active(app) {
        app.ui.tab = "MODEL".into();
    }
}

/// What the canvas frames: the toolpaths of the held slice, else the design's bodies.
/// What the canvas shows, in printer coordinates: the toolpaths on the Slice tab, else the
/// design where its placement puts it. Fit and the view's depth range frame this.
pub fn bounds(app: &SolveApp) -> Option<Aabb3> {
    if app.ui.tab == PREVIEW
        && let Some(held) = app.session.print_slice()
        && let Some(b) = held.sliced.toolpaths.bounds()
    {
        let mut a = Aabb3::default();
        a.add(Vec3::new(f64::from(b.min[0]), f64::from(b.min[1]), f64::from(b.min[2])));
        a.add(Vec3::new(f64::from(b.max[0]), f64::from(b.max[1]), f64::from(b.max[2])));
        return Some(a);
    }
    let b = solidcraft_engine::product_scene::extract(&app.session).ok().map(|s| s.bounds).filter(|b| !b.is_empty())?;
    let o = prepare_offset(app).unwrap_or([0.0; 3]);
    let shift = Vec3::new(f64::from(o[0]), f64::from(o[1]), f64::from(o[2]));
    // A turned part reaches as far as its footprint's half diagonal in any direction.
    let reach = b.size().x.hypot(b.size().y) / 2.0;
    let c = b.center() + shift;
    let mut a = Aabb3::default();
    a.add(Vec3::new(c.x - reach, c.y - reach, b.min.z + shift.z));
    a.add(Vec3::new(c.x + reach, c.y + reach, b.max.z + shift.z));
    Some(a)
}

/// A ribbon button or shortcut of a `print.*` command.
pub fn start(app: &mut SolveApp, id: &str) {
    match id {
        "print.slice" => slice(app),
        "print.save" => save(app),
        "print.upload" => send_to_printer(app, false),
        "print.cancel" => cancel_print(app),
        "print.place" => place(app, json!({"centre": true})),
        _ => {}
    }
}

/// Install (or drop) the printer host for the settings' address, when it changed.
fn sync_printer(app: &mut SolveApp) {
    let url = app.print_workspace.settings.printer_url.trim().trim_end_matches('/').to_string();
    let current = app.session.printer_host().map(|h| h.info().url);
    if url.is_empty() {
        if current.is_some() {
            app.session.set_printer_host(None);
        }
        app.print_workspace.printer_tried = None;
        return;
    }
    if current.as_deref() == Some(url.as_str()) || app.print_workspace.printer_tried.as_deref() == Some(url.as_str()) {
        return;
    }
    app.print_workspace.printer_tried = Some(url.clone());
    let Some(make) = app.services.printer_host.clone() else { return };
    match make(&url) {
        Ok(host) => app.session.set_printer_host(Some(host)),
        Err(e) => {
            app.session.set_printer_host(None);
            app.set_status(e, true);
        }
    }
}

/// Send the held slice to the printer, and start it when `start`.
pub fn send_to_printer(app: &mut SolveApp, start: bool) {
    load_settings(app);
    sync_printer(app);
    match app.run("print.upload", json!({ "start": start })) {
        Ok(v) => {
            app.print_workspace.job = v["job"].as_u64().map(|id| Job::new(id, JobKind::Upload));
            app.set_status(format!("Sending {}", v["name"].as_str().unwrap_or("the G-code")), false);
        }
        Err(e) => app.set_status(e, true),
    }
}

pub fn cancel_print(app: &mut SolveApp) {
    match app.run("print.cancel", json!({})) {
        Ok(v) => app.print_workspace.job = v["job"].as_u64().map(|id| Job::new(id, JobKind::Cancel)),
        Err(e) => app.set_status(e, true),
    }
}

/// The printer's state as the panel words it.
pub(crate) fn state_label(app: &SolveApp, state: &str) -> String {
    let key = match state {
        "standby" => "print.state_standby",
        "printing" => "print.state_printing",
        "paused" => "print.state_paused",
        "complete" => "print.state_complete",
        "cancelled" => "print.state_cancelled",
        "error" => "print.state_error",
        _ => return state.to_string(),
    };
    app.tr(key).to_string()
}

/// Slice the design with the workspace's settings files and show the result.
pub fn slice(app: &mut SolveApp) {
    load_settings(app);
    let files = app.print_workspace.settings.config_files.clone();
    match app.run("print.slice", json!({ "config": files })) {
        Ok(v) => {
            app.print_workspace.top_layer = None;
            if !active(app)
                && let Err(e) = enter(app, true)
            {
                app.set_status(e, true);
            }
            app.ui.tab = PREVIEW.into();
            sync_view(app);
            let layers = v["layers"].as_u64().unwrap_or(0);
            let time = v["time_s"].as_f64().unwrap_or(0.0);
            app.set_status(format!("Sliced: {layers} layers, {}", hms(time)), false);
        }
        Err(e) => app.set_status(e, true),
    }
}

/// Ask where to write the held slice's G-code.
pub fn save(app: &mut SolveApp) {
    if app.session.print_slice().is_none() {
        app.set_status(app.tr("print.not_sliced").to_string(), true);
        return;
    }
    let stem = app.session.path.as_deref().and_then(|p| std::path::Path::new(p).file_stem()).and_then(|s| s.to_str()).unwrap_or("design");
    app.pick_save(&format!("{stem}.gcode"), &["gcode"], move |app, path| {
        let Some(path) = path else { return };
        if let Err(e) = app.run("print.save", json!({ "path": path })) {
            app.set_status(e, true);
        } else {
            app.set_status(format!("Saved {path}"), false);
        }
    });
}

fn settings_path(app: &SolveApp) -> Option<std::path::PathBuf> {
    app.services.graphics_path.as_ref().map(|p| std::path::Path::new(p).with_file_name("print.json"))
}

pub(crate) fn load_settings(app: &mut SolveApp) {
    if app.print_workspace.settings_loaded {
        return;
    }
    app.print_workspace.settings_loaded = true;
    if let Some(path) = settings_path(app)
        && let Ok(text) = std::fs::read_to_string(path)
        && let Ok(settings) = serde_json::from_str::<Settings>(&text)
    {
        app.print_workspace.settings = settings;
    }
}

pub(crate) fn save_settings(app: &mut SolveApp) {
    let Some(path) = settings_path(app) else { return };
    match serde_json::to_string_pretty(&app.print_workspace.settings) {
        Ok(text) => {
            if let Err(e) = std::fs::write(&path, text) {
                app.set_status(format!("{}: {e}", path.display()), true);
            }
        }
        Err(e) => app.set_status(e.to_string(), true),
    }
}

/// Small text that wraps to the panel's width.
fn wrapped_small(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(egui::RichText::new(text).small()).wrap());
}

/// Seconds as `h:mm:ss`.
pub fn hms(seconds: f64) -> String {
    let s = seconds.max(0.0).round() as u64;
    format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

pub fn panel(app: &mut SolveApp, ui: &mut egui::Ui) {
    load_settings(app);
    Container::new(ui.ctx(), Surface::Panel)
        .edge("sc_print_settings", Edge::Right)
        .exact_size(300.0)
        .resizable(false)
        .frame(Container::new(ui.ctx(), Surface::Panel).edge_frame(Tokens::get().panel).inner_margin(10))
        .show(ui, |ui| {
            crate::ui_kit::scope(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if app.ui.tab == PREPARE {
                        prepare_panel(app, ui);
                    } else {
                        slice_panel(app, ui);
                    }
                    ui.separator();
                    if crate::ui_kit::button(ui, app.tr("print.return_design"), crate::ui_kit::Button::Normal).clicked()
                        && let Err(e) = enter(app, false)
                    {
                        app.set_status(e, true);
                    }
                });
            });
        });
    confirm_print(app, ui.ctx());
}

/// Prepare: the profiles, where the part sits on the bed, and the Slice button.
fn prepare_panel(app: &mut SolveApp, ui: &mut egui::Ui) {
    ui.heading(app.tr("print.prepare_title"));
    let n = app.print_workspace.settings.config_files.len();
    let files = if n == 0 { app.tr("print.config_none").to_string() } else { crate::i18n::format("print.config_count", &[("n", &n.to_string())]) };
    ui.label(files);
    let overrides = app.session.doc.print.overlay.len();
    if overrides > 0 {
        ui.small(crate::i18n::format("print.overrides_count", &[("n", &overrides.to_string())]));
    }
    if crate::ui_kit::button(ui, app.tr("print.open_settings"), crate::ui_kit::Button::Normal).clicked() {
        app.start("print.settings.set");
    }
    ui.separator();
    ui.heading(app.tr("print.placement_title"));
    wrapped_small(ui, app.tr("print.placement_hint"));
    let p = app.session.doc.print.placement;
    let (mut x, mut y, mut r) = (p.x, p.y, p.rotation);
    let t = Tokens::get();
    crate::ui_kit::settings_form(ui, t.form_label_width, |ui| {
        let x_label = app.tr("print.place_x").to_string();
        let y_label = app.tr("print.place_y").to_string();
        let r_label = app.tr("print.place_rotation").to_string();
        crate::ui_kit::row(ui, &x_label, |ui| {
            if crate::ui_kit::number_field(ui, &mut x, -500.0..=500.0, 1.0, " mm").changed() {
                place(app, json!({"x": x}));
            }
        });
        crate::ui_kit::row(ui, &y_label, |ui| {
            if crate::ui_kit::number_field(ui, &mut y, -500.0..=500.0, 1.0, " mm").changed() {
                place(app, json!({"y": y}));
            }
        });
        crate::ui_kit::row(ui, &r_label, |ui| {
            if crate::ui_kit::number_field(ui, &mut r, -360.0..=360.0, 5.0, "°").changed() {
                place(app, json!({"rotation": r}));
            }
        });
    });
    if crate::ui_kit::button(ui, app.tr("cmd.print.place"), crate::ui_kit::Button::Enabled(!p.is_default())).clicked() {
        place(app, json!({"centre": true}));
    }
    ui.separator();
    slice_button(app, ui);
}

/// Preview: the result, the toolpath legend, and the way to the printer.
fn slice_panel(app: &mut SolveApp, ui: &mut egui::Ui) {
    ui.heading(app.tr("print.slice_title"));
    slice_button(app, ui);
    match app.session.print_slice().cloned() {
        None => {
            wrapped_small(ui, app.tr("print.slice_from_prepare"));
        }
        Some(held) => {
            ui.separator();
            result_rows(app, ui, &held);
            ui.separator();
            toolpath_controls(app, ui, &held);
            ui.separator();
            printer_rows(app, ui);
        }
    }
}

/// The "start printing?" question, open from Upload and print.
fn confirm_print(app: &mut SolveApp, ctx: &egui::Context) {
    if !app.print_workspace.confirm_print {
        return;
    }
    let name = app.session.print_slice().map(|_| solidcraft_engine::printer_host::gcode_file_name(&app.session.doc.name)).unwrap_or_default();
    let printer = app.session.printer_host().map(|h| h.info().url).unwrap_or_default();
    let title = app.tr("print.confirm_print_title").to_string();
    let body = crate::i18n::format("print.confirm_print_body", &[("name", &name), ("printer", &printer)]);
    let (closed, action) = crate::ui_kit::Confirm::new(egui::Id::new("sc_print_confirm"), "send", &title)
        .tone(crate::ui_kit::Tone::Warning)
        .actions(app.tr("print.confirm_print_ok"), app.tr("ui.cancel"), None)
        .show(ctx, true, |ui| {
            crate::ui_kit::text(ui, &body, crate::ui_kit::Text::Report("print_confirm", false));
        });
    match action {
        crate::ui_kit::Action::Ok => {
            app.print_workspace.confirm_print = false;
            send_to_printer(app, true);
        }
        crate::ui_kit::Action::Cancel | crate::ui_kit::Action::Apply => app.print_workspace.confirm_print = false,
        crate::ui_kit::Action::None if closed => app.print_workspace.confirm_print = false,
        crate::ui_kit::Action::None => {}
    }
}

/// The printer: its address, what it is doing, and the upload and cancel buttons.
/// The printer's Moonraker address, kept between runs; a change installs the host anew.
pub(crate) fn address_rows(app: &mut SolveApp, ui: &mut egui::Ui) {
    load_settings(app);
    ui.label(app.tr("print.printer_url"));
    let before = app.print_workspace.settings.printer_url.clone();
    let hint = app.tr("print.printer_url_hint").to_string();
    crate::ui_kit::bounded_cell_editor(ui, ui.id().with("print_printer_url"), &mut app.print_workspace.settings.printer_url, &hint, false, 256);
    if app.print_workspace.settings.printer_url != before {
        save_settings(app);
    }
    wrapped_small(ui, app.tr("print.printer_url_help"));
    sync_printer(app);
}

/// The printer's state and the upload buttons, on the Slice panel.
fn printer_rows(app: &mut SolveApp, ui: &mut egui::Ui) {
    let t = Tokens::get();
    ui.heading(app.tr("print.printer_title"));
    sync_printer(app);
    let status = match app.run("print.status", json!({})) {
        Ok(v) => v,
        Err(e) => {
            ui.label(e);
            return;
        }
    };
    if status["installed"] != true {
        ui.label(app.tr("print.printer_none"));
        return;
    }
    let state = status["state"].as_str().unwrap_or("");
    if status["reachable"] == true {
        ui.label(state_label(app, state));
        let name = status["filename"].as_str().unwrap_or("");
        if !name.is_empty() && matches!(state, "printing" | "paused") {
            ui.small(name);
            let progress = status["progress"].as_f64().unwrap_or(0.0) as f32;
            crate::ui_kit::progress(ui, "print", progress.clamp(0.0, 1.0));
            ui.small(format!("{:.0} % · {}", progress * 100.0, hms(status["print_duration_s"].as_f64().unwrap_or(0.0))));
        }
        let heater =
            |v: &serde_json::Value| format!("{:.0} / {:.0} °C", v["temperature"].as_f64().unwrap_or(0.0), v["target"].as_f64().unwrap_or(0.0));
        ui.small(format!("{} {} · {} {}", app.tr("print.extruder"), heater(&status["extruder"]), app.tr("print.bed"), heater(&status["bed"])));
    } else {
        let age = status["age_s"].as_f64();
        let text = match age {
            Some(a) => crate::i18n::format("print.unreachable_since", &[("time", &hms(a))]),
            None => app.tr("print.unreachable").to_string(),
        };
        ui.colored_label(t.warning, text);
        if let Some(e) = status["error"].as_str() {
            wrapped_small(ui, e);
        }
    }
    job_line(app, ui, state);
    let can_upload = app.session.print_slice().is_some_and(|h| !h.stale(&app.session.doc));
    ui.horizontal(|ui| {
        if crate::ui_kit::button(ui, app.tr("cmd.print.upload"), crate::ui_kit::Button::Enabled(can_upload)).clicked() {
            send_to_printer(app, false);
        }
        if crate::ui_kit::button(ui, app.tr("print.upload_and_print"), crate::ui_kit::Button::Enabled(can_upload)).clicked() {
            app.print_workspace.confirm_print = true;
        }
    });
    if matches!(state, "printing" | "paused") && crate::ui_kit::button(ui, app.tr("cmd.print.cancel"), crate::ui_kit::Button::Normal).clicked() {
        cancel_print(app);
    }
    // The status line follows the printer while the workspace is open.
    ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
}

fn result_rows(app: &mut SolveApp, ui: &mut egui::Ui, held: &HeldSlice) {
    let t = Tokens::get();
    if held.stale(&app.session.doc) {
        ui.add(egui::Label::new(egui::RichText::new(app.tr("print.stale")).color(t.warning)).wrap());
    }
    ui.heading(app.tr("print.estimate"));
    let paths = &held.sliced.toolpaths;
    egui::Grid::new("print_estimate").num_columns(2).min_col_width(96.0).spacing(vec2(12.0, 4.0)).show(ui, |ui| {
        ui.label(app.tr("print.layers"));
        ui.label(format!("{} ({:.2} mm)", paths.layers.len(), paths.layers.last().map_or(0.0, |l| l.z)));
        ui.end_row();
        ui.label(app.tr("print.time"));
        ui.label(hms(held.sliced.time_s));
        ui.end_row();
        ui.label(app.tr("print.filament"));
        ui.label(format!("{:.2} m · {:.1} g", held.sliced.filament_mm / 1000.0, held.sliced.filament_g));
        ui.end_row();
    });
    ui.small(&held.slicer);
    if !held.sliced.report.refused.is_empty() {
        ui.label(app.tr("print.refused"));
        for r in held.sliced.report.refused.iter().take(12) {
            ui.small(format!("{} = {}: {}", r.key, r.value, r.reason));
        }
    }
    if !held.sliced.report.warnings.is_empty() {
        ui.label(app.tr("print.warnings"));
        for w in held.sliced.report.warnings.iter().take(12) {
            ui.small(w);
        }
    }
    if crate::ui_kit::button(ui, app.tr("cmd.print.save"), crate::ui_kit::Button::Normal).clicked() {
        save(app);
    }
}

fn toolpath_controls(app: &mut SolveApp, ui: &mut egui::Ui, held: &HeldSlice) {
    ui.heading(app.tr("print.toolpaths_title"));
    let paths = &held.sliced.toolpaths;
    let n = paths.layers.len();
    if let Some(layer) = paths.layers.get(shown_layer(app, n).saturating_sub(1)) {
        ui.label(format!("{} {} / {n}", app.tr("print.layer"), shown_layer(app, n)));
        ui.small(format!("z {:.2} mm · {:.2} mm · {} · {} segments", layer.z, layer.height, hms(f64::from(layer.time_s)), layer.segments()));
    }
    // A print past the preview's budget is drawn simpler; say so rather than let it look wrong.
    if app.print_workspace.detail == Some(Detail::Lines) {
        let text = app.tr("print.detail_lines").to_string();
        ui.add(egui::Label::new(egui::RichText::new(text).small().color(Tokens::get().warning)).wrap());
    }
    let travels_label = app.tr("print.show_travels").to_string();
    crate::ui_kit::checkbox(ui, &travels_label, &mut app.print_workspace.show_travels);
    ui.label(app.tr("print.features"));
    if !paths.file_beads.is_empty() {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
            let c = FILE_BEAD_COLOR;
            ui.painter().rect_filled(r, 2.0, Color32::from_rgb(c[0], c[1], c[2]));
            ui.label(app.tr("print.file_beads"));
            ui.small(format!("{:.2} m", paths.file_beads.iter().map(|b| b.length_mm()).sum::<f64>() / 1000.0));
        });
    }
    for total in paths.by_feature() {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
            let c = feature_color(total.feature);
            ui.painter().rect_filled(r, 2.0, Color32::from_rgb(c[0], c[1], c[2]));
            ui.label(total.feature.map_or("Unknown", Feature::label));
            ui.small(format!("{:.2} m", total.length_mm / 1000.0));
        });
    }
}

pub fn show(app: &mut SolveApp, ui: &mut egui::Ui) {
    let rect = ui.max_rect();
    app.viewport.rect = Some(rect);
    crate::viewport::refresh_layout(app, ui.ctx());
    let response = ui.interact(rect, ui.id().with("print_preview"), Sense::click_and_drag());
    load_settings(app);
    bed_info(app);
    sync_view(app);
    crate::printer_dashboard::sync_camera(app);
    let moving = app.ui.tab == PREPARE && move_on_bed(app, ui, rect, &response);
    if !moving {
        crate::navigation::handle(app, ui, rect, &response, crate::navigation::NavCaps::RENDER);
    }
    let t = Tokens::get();
    ui.painter().rect_filled(rect, 0.0, t.viewport_bottom);
    let result = (|| -> Result<(), String> {
        if app.viewport.gpu.is_none() {
            return Err("GPU preview is unavailable on this host.".into());
        }
        upload(app)?;
        let key = app.print_workspace.key.ok_or_else(|| app.tr("print.no_slice").to_string())?;
        let bounds = bounds(app).ok_or("Create a visible solid in Design to print it.")?;
        let camera = app.cam;
        let radius = bounds.diagonal().max(10.0) + bounds.center().dist(camera.target);
        let eye = camera.eye();
        let ppp = ui.ctx().pixels_per_point();
        let slot = app.print_workspace.slot.clone();
        let callback = ViewportCallback(Frame {
            product: true,
            key,
            slot: slot.clone(),
            sketch_key: 0,
            sketch_slot: slot.clone(),
            hl_key: 0,
            hl_slot: slot.clone(),
            pv_key: 0,
            pv_slot: slot,
            pv_hide: false,
            view_proj: camera.view_proj(f64::from(rect.width() / rect.height().max(1.0)), radius).to_f32(),
            back: camera.back().to_f32(),
            size_px: [rect.width() * ppp, rect.height() * ppp],
            // The slider leaves the layers above out of the scene itself (`upload`), so nothing is cut.
            clip: [0.0; 4],
            cap: [0.0; 4],
            images: Vec::new(),
            analysis: [0.0; 8],
            analysis_colors: [0.0; 36],
            access: None,
            outline: false,
            ao_radius: if app.preferences.ambient_occlusion { (bounds.diagonal() * 0.04).clamp(1e-4, 1e6) as f32 } else { 0.0 },
            ao_samples: solidcraft_gpu::view::AO_SAMPLES,
            eye: [eye.x as f32, eye.y as f32, eye.z as f32, if camera.fov > 0.0 { 1.0 } else { 0.0 }],
            ssaa: 1.0,
            hover_tint: [0.0; 3],
            selection_tint: [0.0; 3],
        });
        ui.painter().add(egui_wgpu::Callback::new_paint_callback(rect, callback));
        Ok(())
    })();
    if let Err(error) = result {
        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, error, egui::FontId::proportional(t.font_body), t.text_dim);
    }
    if app.ui.tab == PREVIEW {
        layer_slider(app, ui, rect);
    }
}

/// How far a drag in progress has carried the part, in printer millimetres.
fn drag_shift(app: &SolveApp) -> [f64; 2] {
    app.print_workspace.drag.map_or([0.0; 2], |d| [d.to[0] - d.from[0], d.to[1] - d.from[1]])
}

/// A drag that starts on the part moves it across the bed; the placement is set when the drag
/// ends. True while a drag is on, so the camera stays still.
fn move_on_bed(app: &mut SolveApp, ui: &egui::Ui, rect: egui::Rect, response: &egui::Response) -> bool {
    let proj = crate::viewport::projection(app, rect);
    if response.drag_started_by(egui::PointerButton::Primary)
        && let Some(pos) = response.interact_pointer_pos()
        && over_part(app, &proj, pos)
        && let Some(hit) = bed_hit(&proj, pos)
    {
        let p = app.session.doc.print.placement;
        app.print_workspace.drag = Some(Drag { from: hit, start: [p.x, p.y], to: hit });
    }
    let Some(mut drag) = app.print_workspace.drag else { return false };
    if let Some(pos) = ui.input(|i| i.pointer.latest_pos())
        && let Some(hit) = bed_hit(&proj, pos)
    {
        drag.to = hit;
    }
    app.print_workspace.drag = Some(drag);
    if response.drag_stopped() || !ui.input(|i| i.pointer.primary_down()) {
        app.print_workspace.drag = None;
        let (x, y) = (drag.start[0] + drag.to[0] - drag.from[0], drag.start[1] + drag.to[1] - drag.from[1]);
        if x.is_finite() && y.is_finite() {
            place(app, json!({"x": (x * 10.0).round() / 10.0, "y": (y * 10.0).round() / 10.0}));
        }
    }
    true
}

/// Where a screen point's ray meets the bed plane, in printer coordinates.
fn bed_hit(proj: &crate::viewport::Proj, pos: egui::Pos2) -> Option<[f64; 2]> {
    let (o, d) = proj.ray(pos);
    if d.z.abs() < 1e-9 {
        return None;
    }
    let t = -o.z / d.z;
    if !t.is_finite() || t < 0.0 {
        return None;
    }
    Some([o.x + d.x * t, o.y + d.y * t])
}

/// The eight corners of the part's bounds where it stands, in printer coordinates.
fn placed_corners(app: &SolveApp) -> Option<[Vec3; 8]> {
    let b = solidcraft_engine::product_scene::extract(&app.session).ok()?.bounds;
    if b.is_empty() {
        return None;
    }
    let o = prepare_offset(app)?;
    let (ox, oy, oz) = (f64::from(o[0]), f64::from(o[1]), f64::from(o[2]));
    let mut out = [Vec3::ZERO; 8];
    for (i, c) in out.iter_mut().enumerate() {
        let x = if i & 1 == 0 { b.min.x } else { b.max.x };
        let y = if i & 2 == 0 { b.min.y } else { b.max.y };
        let z = if i & 4 == 0 { b.min.z } else { b.max.z };
        *c = Vec3::new(x + ox, y + oy, z + oz);
    }
    Some(out)
}

/// Whether a screen point lies over the part's bounds, as they project.
fn over_part(app: &SolveApp, proj: &crate::viewport::Proj, pos: egui::Pos2) -> bool {
    let Some(corners) = placed_corners(app) else { return false };
    let mut rect = egui::Rect::NOTHING;
    for c in corners {
        if let Some(p) = proj.to_screen(c) {
            rect.extend_with(p);
        }
    }
    rect.is_positive() && rect.contains(pos)
}

/// The highest layer shown, 1-based, for a slice of `n` layers.
fn shown_layer(app: &SolveApp, n: usize) -> usize {
    app.print_workspace.top_layer.map_or(n, |i| i + 1).clamp(1, n.max(1))
}

/// The vertical layer slider on the canvas's right edge: the top is the last layer, the bottom
/// the first, as the print grows. Registered after the camera input, so a drag on it never orbits.
fn layer_slider(app: &mut SolveApp, ui: &mut egui::Ui, rect: egui::Rect) {
    let Some(held) = app.session.print_slice().cloned() else { return };
    let n = held.sliced.toolpaths.layers.len();
    if n < 2 {
        return;
    }
    // The part of the canvas no card covers, so the slider stays clear of the floating panel.
    let free = crate::viewport_layout::free_rect(crate::shell::content_rect(app, rect), &crate::shell::covered_rects(app));
    let lane = egui::Rect::from_min_max(egui::pos2(free.right() - 44.0, free.top() + 36.0), egui::pos2(free.right() - 16.0, free.bottom() - 36.0));
    if lane.height() < 80.0 {
        return;
    }
    crate::scenario::publish_target("print:layer_slider", lane);
    let mut shown = shown_layer(app, n);
    let response = crate::ui_kit::vertical_slider(ui, lane, &mut shown, 1..=n);
    let shown = shown.clamp(1, n);
    app.print_workspace.top_layer = (shown < n).then_some(shown - 1);
    layer_readout(ui, &lane, &response.rect, shown, n, held.sliced.toolpaths.layers.get(shown - 1).map_or(0.0, |l| l.z));
}

/// The layer number and height beside the slider's thumb.
fn layer_readout(ui: &mut egui::Ui, lane: &egui::Rect, slider: &egui::Rect, shown: usize, n: usize, z: f32) {
    let t = Tokens::get();
    // The thumb's centre, as the kit places it: a handle radius inside each end of the lane.
    let inset = ui.text_style_height(&egui::TextStyle::Body).max(lane.width()) / 2.5;
    let y = (slider.bottom() - inset) - ((shown - 1) as f32 / (n.max(2) - 1) as f32) * (slider.height() - 2.0 * inset).max(0.0);
    let x = lane.center().x;
    let painter = ui.painter();
    let text = format!("{shown}\n{z:.2} mm");
    let galley = painter.layout_no_wrap(text, egui::FontId::proportional(t.font_small), t.text);
    let label =
        egui::Rect::from_min_size(egui::pos2(x - 16.0 - galley.size().x - 8.0, y - galley.size().y / 2.0 - 3.0), galley.size() + vec2(8.0, 6.0));
    painter.rect_filled(label, 4.0, t.overlay);
    painter.galley(label.min + vec2(4.0, 3.0), galley, t.text);
}

/// Build the scene for what the canvas shows, when that changed.
fn upload(app: &mut SolveApp) -> Result<(), String> {
    let p = app.session.doc.print.placement;
    let placement = [(p.x * 1000.0).round() as i64, (p.y * 1000.0).round() as i64, (p.rotation * 1000.0).round() as i64];
    let bed = app.print_workspace.bed.as_ref().map_or([0; 4], |b| b.rect.map(|v| (v * 10.0).round() as i32));
    let shift = drag_shift(app);
    let drag = [(shift[0] * 10.0).round() as i64, (shift[1] * 10.0).round() as i64];
    let source = match app.session.print_slice() {
        Some(held) if app.ui.tab == PREVIEW => Source::Toolpaths {
            slice: Arc::as_ptr(held) as usize,
            travels: app.print_workspace.show_travels,
            shown: shown_layer(app, held.sliced.toolpaths.layers.len()),
        },
        _ => Source::Prepared { revision: app.session.revision, world: Arc::as_ptr(&app.session.world_state()) as usize, placement, bed, drag },
    };
    if app.print_workspace.uploaded == Some(source) {
        return Ok(());
    }
    let scene = match (source, app.session.print_slice().cloned()) {
        (Source::Toolpaths { travels, shown, .. }, Some(held)) => {
            let layered = match &app.print_workspace.layered {
                Some((made_for, with_travels, layered)) if Arc::ptr_eq(made_for, &held) && *with_travels == travels => layered.clone(),
                _ => {
                    let layered = Arc::new(Layered::build(&held, travels)?);
                    app.print_workspace.layered = Some((held.clone(), travels, layered.clone()));
                    layered
                }
            };
            let mut scene = layered.shown(shown);
            app.print_workspace.detail = Some(layered.detail);
            shell(app, &mut scene, held.sliced.offset);
            scene
        }
        _ => prepared_scene(app)?,
    };
    if let Ok(mut slot) = app.print_workspace.slot.lock() {
        *slot = Some(scene);
    }
    app.print_workspace.key = Some(app.print_workspace.key.map_or(1, |k| k.wrapping_add(1)));
    app.print_workspace.uploaded = Some(source);
    Ok(())
}

/// A faint see-through shell of the whole design over the toolpaths, moved onto the bed by the
/// slice's placement, so the layers below the slider still hint at the part above them.
/// Reference geometry: the layer cut never removes it.
fn shell(app: &SolveApp, gpu: &mut GpuScene, offset: [f32; 3]) {
    let Ok(scene) = solidcraft_engine::product_scene::extract(&app.session) else { return };
    let t = Tokens::get();
    let c = gpu.color([t.text_dim.r(), t.text_dim.g(), t.text_dim.b(), SHELL_ALPHA]);
    // Turned as the slice turned it: about the footprint's centre, before the offset.
    let about = scene.bounds.center().to_f32();
    let (sin, cos) = (app.session.doc.print.placement.rotation as f32).to_radians().sin_cos();
    let mut bytes = 0usize;
    for body in &scene.meshes {
        for triangle in &body.mesh.triangles {
            bytes += TRI_BYTES;
            if bytes > MAX_GEOMETRY_BYTES / 4 {
                return;
            }
            // A hair inside the surface the beads lie on, so the shell never shares a depth
            // with the outer wall (two surfaces at one depth flicker as the view moves).
            let inset = |i: &u32| {
                let p = body.mesh.positions.get(*i as usize)?.to_f32();
                let n = body.mesh.normals.get(*i as usize).map_or([0.0; 3], |n| n.to_f32());
                Some([p[0] - n[0] * SHELL_INSET, p[1] - n[1] * SHELL_INSET, p[2] - n[2] * SHELL_INSET])
            };
            let corners = triangle.map(|i| inset(&i));
            let [Some(a), Some(b), Some(d)] = corners else { continue };
            for p in [a, b, d] {
                let (dx, dy) = (p[0] - about[0], p[1] - about[1]);
                let x = about[0] + dx * cos - dy * sin;
                let y = about[1] + dx * sin + dy * cos;
                gpu.guide_tri([x + offset[0], y + offset[1], p[2] + offset[2]], c);
            }
        }
    }
}

/// Opacity of the shell, 0 to 255.
const SHELL_ALPHA: u8 = 24;
/// How far inside the part's surface the shell is drawn, mm, so it never shares a depth with
/// the outer wall's beads.
const SHELL_INSET: f32 = 0.1;

/// The design's bodies in a neutral grey, with the bed as a grid when one is known.
/// The design's bodies where the placement puts them, turned about their footprint's centre and
/// standing on the bed the settings describe.
fn prepared_scene(app: &SolveApp) -> Result<GpuScene, String> {
    let scene = solidcraft_engine::product_scene::extract(&app.session).map_err(|e| e.to_string())?;
    let mut gpu = GpuScene::default();
    let grey = gpu.color([0xb4, 0xba, 0xc4, 255]);
    let mut offset = prepare_offset(app).unwrap_or([0.0; 3]);
    // While the part is dragged it moves with the pointer; the placement follows at the end.
    let shift = drag_shift(app);
    offset[0] += shift[0] as f32;
    offset[1] += shift[1] as f32;
    let c = scene.bounds.center().to_f32();
    let (sin, cos) = (app.session.doc.print.placement.rotation as f32).to_radians().sin_cos();
    let turn = |v: [f32; 3], about: [f32; 2]| {
        let (dx, dy) = (v[0] - about[0], v[1] - about[1]);
        [about[0] + dx * cos - dy * sin, about[1] + dx * sin + dy * cos, v[2]]
    };
    let mut bytes = 0usize;
    for body in &scene.meshes {
        for triangle in &body.mesh.triangles {
            bytes += TRI_BYTES;
            if bytes > MAX_GEOMETRY_BYTES {
                return Err("the design is past the preview's geometry budget".into());
            }
            for i in *triangle {
                let p = body.mesh.positions.get(i as usize).ok_or("invalid product triangle")?.to_f32();
                let n = body.mesh.normals.get(i as usize).ok_or("invalid product triangle")?.to_f32();
                let p = turn(p, [c[0], c[1]]);
                let n = turn(n, [0.0, 0.0]);
                gpu.tri([p[0] + offset[0], p[1] + offset[1], p[2] + offset[2]], n, grey);
            }
        }
    }
    if let Some(b) = &app.print_workspace.bed {
        bed(&mut gpu, b.rect);
    }
    Ok(gpu)
}

/// What the toolpaths cost: as beads, one placement per straight run, joined corner and open
/// end ([`Instancer`]); as lines, one line per straight run.
struct Cost {
    segments: usize,
    /// Corners that get a join ([`join_wanted`]).
    joins: usize,
    /// Open path ends.
    caps: usize,
}

impl Cost {
    fn of(paths: &Toolpaths) -> Cost {
        let mut cost = Cost { segments: 0, joins: 0, caps: 0 };
        for path in paths.layers.iter().flat_map(|l| &l.paths) {
            let steps: Vec<[f32; 2]> = path.points.windows(2).filter_map(|w| direction(w[0], w[1]).map(|(d, _)| d)).collect();
            cost.segments += steps.len();
            let closed = path.points.len() > 2 && path.points.first() == path.points.last();
            let seam = if closed { steps.last().copied().zip(steps.first().copied()) } else { None };
            cost.joins += steps.windows(2).map(|w| (w[0], w[1])).chain(seam).filter(|(d0, d1)| join_wanted(turn(*d0, *d1))).count();
            if !closed && !steps.is_empty() {
                cost.caps += 2;
            }
        }
        cost
    }

    fn bytes(&self, detail: Detail) -> usize {
        match detail {
            Detail::Beads => (self.segments + self.joins + self.caps).saturating_mul(crate::gpu::INSTANCE_SIZE),
            Detail::Lines => self.segments.saturating_mul(LINE_BYTES),
        }
    }
}

/// The toolpaths as beads: each segment a bead with a flat top and half-round sides, fans of
/// the same profile on the outside of every corner so the bead turns without a notch, and caps
/// on open ends, all placements of shared pieces ([`Instancer`]). Past the budget the beads
/// become lines. Built once for every layer, with where each layer ends: the layer slider shows
/// the first layers whole ([`Layered::shown`]), never cut through by height, and the detail,
/// chosen for the whole print, stays put while it moves.
struct Layered {
    /// Everything but the placements and the lines: the bed, the palette.
    base: GpuScene,
    /// The shared pieces with every layer's placements, in layer order.
    meshes: Vec<crate::gpu::InstancedMesh>,
    /// Every layer's lines (travel moves, or the beads as lines), in layer order.
    lines: Vec<u8>,
    /// After each layer: the bytes of each piece's placements (by its index in `meshes`; a piece
    /// first placed later has none yet) and of the lines.
    ends: Vec<(Vec<usize>, usize)>,
    detail: Detail,
}

impl Layered {
    fn build(held: &HeldSlice, travels: bool) -> Result<Layered, String> {
        let paths: &Toolpaths = &held.sliced.toolpaths;
        let travel_count: usize = paths.layers.iter().map(|l| l.paths.len().saturating_sub(1)).sum();
        let travel_bytes = if travels { travel_count * LINE_BYTES } else { 0 };
        let cost = Cost::of(paths);
        let detail = [Detail::Beads, Detail::Lines]
            .into_iter()
            .find(|d| cost.bytes(*d).saturating_add(travel_bytes) <= MAX_GEOMETRY_BYTES)
            .ok_or_else(|| format!("{} segments are past the preview's geometry budget", cost.segments))?;
        let mut gpu = GpuScene::default();
        let mut inst = Instancer::default();
        bed(&mut gpu, held.sliced.bed);
        let first_height = paths.layers.first().map_or(0.2, |l| l.height);
        for bead in &paths.file_beads {
            file_bead(&mut gpu, &mut inst, bead, first_height, detail);
        }
        let mut ends = Vec::with_capacity(paths.layers.len());
        for layer in &paths.layers {
            layer_beads(&mut gpu, &mut inst, layer, detail);
            if travels {
                // A little above the beads, so the line never fights with the top faces.
                let z = layer.z + TRAVEL_LIFT;
                let travel = gpu.color(TRAVEL_COLOR);
                for (a, b) in layer.travels() {
                    gpu.line([a[0], a[1], z], [b[0], b[1], z], travel, 1.0, false);
                }
            }
            ends.push((inst.meshes.iter().map(|m| m.instances.len()).collect(), gpu.lines.len()));
        }
        let lines = std::mem::take(&mut gpu.lines);
        Ok(Layered { base: gpu, meshes: inst.meshes, lines, ends, detail })
    }

    /// The scene of the first `shown` layers (all of them past the last).
    fn shown(&self, shown: usize) -> GpuScene {
        let mut out = self.base.clone();
        let Some((meshes, lines)) = shown.checked_sub(1).and_then(|i| self.ends.get(i.min(self.ends.len().saturating_sub(1)))) else {
            return out;
        };
        out.lines = self.lines.get(..*lines).unwrap_or_default().to_vec();
        out.instanced = self
            .meshes
            .iter()
            .zip(meshes)
            .filter(|(_, end)| **end > 0)
            .map(|(m, end)| crate::gpu::InstancedMesh {
                key: m.key,
                tris: m.tris.clone(),
                instances: m.instances.get(..*end).unwrap_or_default().to_vec(),
                look: m.look,
                levels: m.levels.clone(),
            })
            .collect();
        out
    }
}

/// The beads as placements of a few shared pieces, each made once per profile in its own frame
/// (along +x, sideways +y, its top at z = 0) and laid by its placement: a straight run of unit
/// length stretched along the path, a corner's fan for its angle in whole degrees, an end cap.
/// A placement turns a piece to the path's direction and moves it to its point and height; the
/// stretch only lengthens a straight run, whose faces all run along it, so no normal changes.
#[derive(Default)]
struct Instancer {
    /// The pieces in the order they were first placed.
    meshes: Vec<crate::gpu::InstancedMesh>,
    /// Each piece's index in `meshes`, by key.
    index: std::collections::HashMap<u64, usize>,
}

/// Which shared piece of a profile.
#[derive(Hash)]
enum Piece {
    Run,
    /// A corner turning this many degrees (a multiple of [`FAN_STEP_DEGREES`]), to the left or not.
    Join { degrees: u16, left: bool },
    /// An end facing along the path (`true`, the path's last point) or back along it.
    Cap { forward: bool },
}

impl Instancer {
    fn place(&mut self, prof: &Profile, piece: Piece, rows: [[f32; 4]; 3], c: PaletteIndex) {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        prof.key().hash(&mut h);
        piece.hash(&mut h);
        let key = h.finish();
        let i = *self.index.entry(key).or_insert_with(|| {
            self.meshes.push(crate::gpu::InstancedMesh {
                key,
                tris: Arc::new(shared(prof, &piece_tris(prof, &piece))),
                instances: Vec::new(),
                look: crate::gpu::InstanceLook::Opaque,
                levels: Vec::new(),
            });
            self.meshes.len() - 1
        });
        let Some(mesh) = self.meshes.get_mut(i) else { return };
        mesh.place(rows, c);
    }

    /// A straight run from `a` to `b`.
    fn run(&mut self, prof: &Profile, a: [f32; 2], b: [f32; 2], c: PaletteIndex) {
        let n = [a[1] - b[1], b[0] - a[0]];
        self.place(prof, Piece::Run, [[b[0] - a[0], unit2(n)[0], 0.0, a[0]], [b[1] - a[1], unit2(n)[1], 0.0, a[1]], [0.0, 0.0, 1.0, prof.top]], c);
    }

    /// The outside of the corner at `v` between a step going `d0` and one going `d1`.
    fn join(&mut self, prof: &Profile, v: [f32; 2], d0: [f32; 2], d1: [f32; 2], c: PaletteIndex) {
        let angle = turn(d0, d1);
        if !join_wanted(angle) {
            return;
        }
        let degrees = fan_degrees(angle);
        let left = d0[0] * d1[1] - d0[1] * d1[0] > 0.0;
        self.place(prof, Piece::Join { degrees, left }, turned(d0, v, prof.top), c);
    }

    /// The end of an open bead at `p`, the path going `d` there; `forward` for its last point.
    fn cap(&mut self, prof: &Profile, p: [f32; 2], d: [f32; 2], forward: bool, c: PaletteIndex) {
        self.place(prof, Piece::Cap { forward }, turned(d, p, prof.top), c);
    }
}

/// The rows of a placement that turns +x to `d` (and +y to its left) and moves the origin to
/// `at`, `top` high.
fn turned(d: [f32; 2], at: [f32; 2], top: f32) -> [[f32; 4]; 3] {
    [[d[0], -d[1], 0.0, at[0]], [d[1], d[0], 0.0, at[1]], [0.0, 0.0, 1.0, top]]
}

/// One triangle: its corners (wound so the normal is its outward side) and its normal.
type Tri = ([[f32; 3]; 3], [f32; 3]);

/// A piece's triangles in its own frame, at the profile's height on the plate.
fn piece_tris(prof: &Profile, piece: &Piece) -> Vec<Tri> {
    let mut out = Vec::new();
    match *piece {
        Piece::Run => {
            let n = [0.0, 1.0];
            for k in 0..prof.edges() {
                let (p0, p1, p2, p3) = (prof.at([0.0, 0.0], n, k), prof.at([1.0, 0.0], n, k), prof.at([1.0, 0.0], n, k + 1), prof.at([0.0, 0.0], n, k + 1));
                quad(&mut out, [p0, p1, p2, p3], prof.face_normal(n, k));
            }
        }
        Piece::Join { degrees, left } => {
            let (s, c) = f32::from(degrees).to_radians().sin_cos();
            join_tris(&mut out, prof, [0.0, 0.0], [1.0, 0.0], [c, if left { s } else { -s }]);
        }
        Piece::Cap { forward } => {
            let facing = if forward { 1.0 } else { -1.0 };
            let normal = [facing, 0.0, 0.0];
            for k in 1..prof.ring.len().saturating_sub(1) {
                let corners = [prof.at([0.0, 0.0], [0.0, 1.0], 0), prof.at([0.0, 0.0], [0.0, 1.0], k), prof.at([0.0, 0.0], [0.0, 1.0], k + 1)];
                out.push((wound(corners, normal), normal));
            }
        }
    }
    out
}

/// The outside of a corner at `v`, between a step going `d0` and one going `d1`: the ring's
/// faces on that side swept round the corner, with wedges on the top and the bottom.
fn join_tris(out: &mut Vec<Tri>, prof: &Profile, v: [f32; 2], d0: [f32; 2], d1: [f32; 2]) {
    let cross = d0[0] * d1[1] - d0[1] * d1[0];
    let angle = turn(d0, d1);
    // A left turn (cross > 0) leaves the gap on the right side, and the other way round.
    let side = if cross > 0.0 { -1.0 } else { 1.0 };
    let n0 = [-d0[1], d0[0]];
    let steps = join_steps(angle);
    // The side normal sweeps from n0 to n1 through the outside of the turn.
    let sweep = if side > 0.0 { -angle } else { angle };
    let normal_at = |t: f32| {
        let (s, co) = (sweep * t).sin_cos();
        [n0[0] * co - n0[1] * s, n0[0] * s + n0[1] * co]
    };
    let run = prof.side(side);
    let (highest, lowest) = prof.extremes(run.clone());
    let centre_top = [v[0], v[1], prof.top];
    let centre_bottom = [v[0], v[1], prof.bottom];
    for i in 0..steps {
        let (na, nb) = (normal_at(i as f32 / steps as f32), normal_at((i + 1) as f32 / steps as f32));
        let nm = unit2([(na[0] + nb[0]) * 0.5, (na[1] + nb[1]) * 0.5]);
        for k in run.start..run.end.saturating_sub(1) {
            quad(out, [prof.at(v, na, k), prof.at(v, nb, k), prof.at(v, nb, k + 1), prof.at(v, na, k + 1)], prof.face_normal(nm, k));
        }
        let up = [0.0, 0.0, 1.0];
        out.push((wound([centre_top, prof.at(v, na, highest), prof.at(v, nb, highest)], up), up));
        if prof.bottom_face {
            let down = [0.0, 0.0, -1.0];
            out.push((wound([centre_bottom, prof.at(v, na, lowest), prof.at(v, nb, lowest)], down), down));
        }
    }
}

/// Two triangles for the quad `p0 p1 p2 p3`, wound so `n` is their outward normal.
fn quad(out: &mut Vec<Tri>, [p0, p1, p2, p3]: [[f32; 3]; 4], n: [f32; 3]) {
    out.push((wound([p0, p1, p2], n), n));
    out.push((wound([p0, p2, p3], n), n));
}

/// A piece's triangles as a shared mesh in its own frame: its top at z = 0.
fn shared(prof: &Profile, tris: &[Tri]) -> crate::gpu::SharedTris {
    let mut out = crate::gpu::SharedTris::default();
    let mut lo = [f32::INFINITY; 3];
    let mut hi = [f32::NEG_INFINITY; 3];
    for (corners, n) in tris {
        for p in corners {
            let p = [p[0], p[1], p[2] - prof.top];
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
            let index = u32::try_from(out.vertices.len() / crate::gpu::SHARED_SIZE).unwrap_or(u32::MAX);
            for v in p.iter().chain(n) {
                out.vertices.extend_from_slice(&v.to_le_bytes());
            }
            out.indices.push(index);
        }
    }
    out.bounds = [lo, hi];
    out
}

/// How far above a layer's top its travel moves are drawn, mm.
const TRAVEL_LIFT: f32 = 0.05;
/// Widest turn one join step spans; sharper corners get more steps so the fan stays round.
const JOIN_STEP: f32 = std::f32::consts::FRAC_PI_4;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Detail {
    /// The full profile.
    Beads,
    /// One line per straight run, for a print past the budget even as placements.
    Lines,
}

/// The bead's cross-section: a ring of points in (sideways, height) about the path, going
/// round counter-clockwise seen along the path (up the right side, across the top, down the
/// left side, across the bottom). The ring is a stadium, flat on top and underneath with
/// half-round sides, the shape a laid bead takes: `half` to each side, the layer tall.
struct Profile {
    ring: Vec<[f32; 2]>,
    /// Whether the face underneath is drawn. A layer's bead sits on the layer below (or the
    /// bed), whose top is at the same height: drawing both would put two faces at one depth,
    /// which flicker where the layer clip exposes them. The purge line on the bed keeps its.
    bottom_face: bool,
    /// The ring's indices on the right (+1) side and the left (-1) side, each a run along the
    /// ring, so the faces between neighbours are the ring's own.
    right: std::ops::Range<usize>,
    left: std::ops::Range<usize>,
    top: f32,
    bottom: f32,
}

impl Profile {
    /// Names the profile's shape (not its height on the plate): profiles with the same key share
    /// their pieces. Rounded to a tenth of a micrometre, so layers of one height share them
    /// whatever their tops' rounding.
    fn key(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        let tenth_um = |v: f32| (v * 1e4).round() as i64;
        for [r, z] in &self.ring {
            tenth_um(*r).hash(&mut h);
            tenth_um(z - self.top).hash(&mut h);
        }
        self.bottom_face.hash(&mut h);
        h.finish()
    }

    fn new(top: f32, bottom: f32, half: f32, bottom_face: bool) -> Self {
        let mid = f32::midpoint(top, bottom);
        let b = ((top - bottom) * 0.5).max(1e-4);
        let a = half.min(b);
        let flat = (half - a).max(0.0);
        // A point of the arc on one side, `t` degrees up from the bead's widest line.
        let arc = |t: f32, side: f32| -> [f32; 2] {
            let (sin, cos) = t.to_radians().sin_cos();
            [side * (flat + a * cos), mid + b * sin]
        };
        Self {
            ring: vec![
                arc(-90.0, 1.0),
                arc(-45.0, 1.0),
                arc(0.0, 1.0),
                arc(45.0, 1.0),
                arc(90.0, 1.0),
                arc(90.0, -1.0),
                arc(45.0, -1.0),
                arc(0.0, -1.0),
                arc(-45.0, -1.0),
                arc(-90.0, -1.0),
            ],
            bottom_face,
            right: 0..5,
            left: 5..10,
            top,
            bottom,
        }
    }

    /// How many faces a straight run has: the ring's edges, less the closing one underneath
    /// when that face is not drawn.
    fn edges(&self) -> usize {
        if self.bottom_face { self.ring.len() } else { self.ring.len().saturating_sub(1) }
    }

    /// Ring point `k` at `p`, where `n` is the unit vector to the path's left.
    fn at(&self, p: [f32; 2], n: [f32; 2], k: usize) -> [f32; 3] {
        let [r, z] = self.ring[k % self.ring.len()];
        [p[0] + n[0] * r, p[1] + n[1] * r, z]
    }

    /// The outward normal of the face between ring points `k` and `k + 1`.
    fn face_normal(&self, n: [f32; 2], k: usize) -> [f32; 3] {
        let a = self.ring[k % self.ring.len()];
        let b = self.ring[(k + 1) % self.ring.len()];
        let (dr, dz) = (b[0] - a[0], b[1] - a[1]);
        unit([n[0] * dz, n[1] * dz, -dr])
    }

    /// The ring's run on one side of the path.
    fn side(&self, side: f32) -> std::ops::Range<usize> {
        if side > 0.0 { self.right.clone() } else { self.left.clone() }
    }

    /// The highest and the lowest point of a run.
    fn extremes(&self, run: std::ops::Range<usize>) -> (usize, usize) {
        let highest = run.clone().max_by(|&i, &j| self.ring[i][1].total_cmp(&self.ring[j][1])).unwrap_or(0);
        let lowest = run.min_by(|&i, &j| self.ring[i][1].total_cmp(&self.ring[j][1])).unwrap_or(0);
        (highest, lowest)
    }
}

fn unit2(v: [f32; 2]) -> [f32; 2] {
    let l = v[0].hypot(v[1]);
    if l > 1e-12 { [v[0] / l, v[1] / l] } else { [1.0, 0.0] }
}

fn unit(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if l > 1e-12 { [v[0] / l, v[1] / l, v[2] / l] } else { [0.0, 0.0, 1.0] }
}

/// The unit direction and left normal of `a → b`, or `None` for a zero-length step.
fn direction(a: [f32; 2], b: [f32; 2]) -> Option<([f32; 2], [f32; 2])> {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len = dx.hypot(dy);
    (len > 1e-6).then(|| ([dx / len, dy / len], [-dy / len, dx / len]))
}

fn layer_beads(gpu: &mut GpuScene, inst: &mut Instancer, layer: &Layer, detail: Detail) {
    let top = layer.z;
    let bottom = (layer.z - layer.height).max(0.0);
    for path in &layer.paths {
        let c = gpu.color(feature_color(path.feature));
        let half = bead_half(path.width_mm);
        if detail == Detail::Lines {
            for w in path.points.windows(2) {
                if direction(w[0], w[1]).is_some() {
                    gpu.line([w[0][0], w[0][1], top], [w[1][0], w[1][1], top], c, 1.5, false);
                }
            }
            continue;
        }
        bead_path(inst, &Profile::new(top, bottom, half, false), &path.points, c);
    }
}

/// One path of bead: its straight runs, a fan on the outside of each corner, and either caps on
/// its two ends or, for a closed loop, a fan at its seam.
fn bead_path(inst: &mut Instancer, prof: &Profile, pts: &[[f32; 2]], c: PaletteIndex) {
    let closed = pts.len() > 2 && pts.first() == pts.last();
    let mut previous: Option<[f32; 2]> = None;
    for w in pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let Some((d, _)) = direction(a, b) else { continue };
        inst.run(prof, a, b, c);
        match previous {
            Some(d0) => inst.join(prof, a, d0, d, c),
            None if !closed => inst.cap(prof, a, d, false, c),
            None => {}
        }
        previous = Some(d);
    }
    let Some(d_last) = previous else { return };
    if closed {
        // The loop's seam: the join between its last and first steps.
        if let Some(((d_first, _), last)) = pts.windows(2).find_map(|w| direction(w[0], w[1])).zip(pts.last()) {
            inst.join(prof, *last, d_last, d_first, c);
        }
    } else if let Some(last) = pts.last() {
        inst.cap(prof, *last, d_last, true, c);
    }
}

/// A bead the start or end template lays (the purge line): drawn as the layers' beads are, in
/// runs of points at one height, each with its corners and ends.
fn file_bead(gpu: &mut GpuScene, inst: &mut Instancer, bead: &solidcraft_engine::print::FileBead, first_height: f32, detail: Detail) {
    let c = gpu.color(FILE_BEAD_COLOR);
    let half = bead_half(bead.width_mm);
    if detail == Detail::Lines {
        for w in bead.points.windows(2) {
            if direction([w[0][0], w[0][1]], [w[1][0], w[1][1]]).is_some() {
                gpu.line(w[0], w[1], c, 1.5, false);
            }
        }
        return;
    }
    let mut run: Vec<[f32; 2]> = Vec::new();
    let mut run_z: Option<f32> = None;
    let flush = |inst: &mut Instancer, run: &mut Vec<[f32; 2]>, z: f32| {
        if run.len() >= 2 {
            let bottom = (z - z.min(first_height).max(0.05)).max(0.0);
            bead_path(inst, &Profile::new(z, bottom, half, true), run, c);
        }
        run.clear();
    };
    for p in &bead.points {
        match run_z {
            Some(z) if (p[2] - z).abs() <= 1e-3 => run.push([p[0], p[1]]),
            Some(z) => {
                flush(inst, &mut run, z);
                run.push([p[0], p[1]]);
                run_z = Some(p[2]);
            }
            None => {
                run.push([p[0], p[1]]);
                run_z = Some(p[2]);
            }
        }
    }
    if let Some(z) = run_z {
        flush(inst, &mut run, z);
    }
}

/// The angle between two unit directions, radians.
fn turn(d0: [f32; 2], d1: [f32; 2]) -> f32 {
    (d0[0] * d1[0] + d0[1] * d1[1]).clamp(-1.0, 1.0).acos()
}

/// Whether a corner turning `angle` gets a join: every corner that turns at all. A gap left on
/// the outside of a turn, however narrow, shows as a dark seam across the bead up close.
fn join_wanted(angle: f32) -> bool {
    angle.is_finite() && angle >= STRAIGHT_ON
}

/// The fan for a corner turning `angle`, in whole degrees: rounded up to a multiple of
/// [`FAN_STEP_DEGREES`]. A fan that turns too far is hidden: past the true turn it lies within
/// the bead's half width of the next run's centre line, inside that run's bead. Rounding up keeps
/// the outside of every turn covered, and the steps keep the shared fans few.
fn fan_degrees(angle: f32) -> u16 {
    let step = f32::from(FAN_STEP_DEGREES);
    ((angle.to_degrees() / step).ceil() as u16 * FAN_STEP_DEGREES).clamp(FAN_STEP_DEGREES, 180)
}

/// The fans' angle step, degrees.
const FAN_STEP_DEGREES: u16 = 2;

/// Half a bead `width` wide, the width rounded to [`WIDTH_STEP`] so beads of nearly one width
/// share their pieces (gap fill and internal solid infill vary their widths continuously, and
/// one shared piece per width costs the GPU a buffer each).
fn bead_half(width: f32) -> f32 {
    ((width / WIDTH_STEP).round() * WIDTH_STEP * 0.5).max(0.05)
}

/// The step bead widths are rounded to, mm: a bead is drawn at most half of it off its width.
const WIDTH_STEP: f32 = 0.01;

/// A turn this small (radians) is a straight run on: about 0.006°.
const STRAIGHT_ON: f32 = 1e-4;

fn join_steps(angle: f32) -> usize {
    ((angle / JOIN_STEP - 1e-4).ceil() as usize).clamp(1, 8)
}

/// The corners of a triangle in the order that makes `n` its outward side.
fn wound([a, b, c]: [[f32; 3]; 3], n: [f32; 3]) -> [[f32; 3]; 3] {
    let (u, v) = ([b[0] - a[0], b[1] - a[1], b[2] - a[2]], [c[0] - a[0], c[1] - a[1], c[2] - a[2]]);
    let cross = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
    if cross[0] * n[0] + cross[1] * n[1] + cross[2] * n[2] < 0.0 { [a, c, b] } else { [a, b, c] }
}

/// The bed outline and a 10 mm grid under everything.
fn bed(gpu: &mut GpuScene, b: [f32; 4]) {
    let [x0, y0, x1, y1] = b;
    if !(x1 > x0 && y1 > y0) || !b.iter().all(|v| v.is_finite()) {
        return;
    }
    let t = Tokens::get();
    let line = |c: Color32| [c.r(), c.g(), c.b(), 255];
    let (minor, major) = (gpu.color(line(t.grid)), gpu.color(line(t.grid_major)));
    let mut x = (x0 / 10.0).ceil() * 10.0;
    while x <= x1 {
        gpu.under_line([x, y0, 0.0], [x, y1, 0.0], minor, 1.0);
        x += 10.0;
    }
    let mut y = (y0 / 10.0).ceil() * 10.0;
    while y <= y1 {
        gpu.under_line([x0, y, 0.0], [x1, y, 0.0], minor, 1.0);
        y += 10.0;
    }
    for (a, b) in [([x0, y0], [x1, y0]), ([x1, y0], [x1, y1]), ([x1, y1], [x0, y1]), ([x0, y1], [x0, y0])] {
        gpu.under_line([a[0], a[1], 0.0], [b[0], b[1], 0.0], major, 1.5);
    }
}

#[cfg(test)]
#[path = "print_workspace_tests.rs"]
mod tests;

#[cfg(test)]
mod job_tests {
    use super::{JobKind, job_text};
    use std::time::Duration;

    #[test]
    fn a_cancel_is_judged_by_the_printers_state_not_the_requests_answer() {
        let soon = Duration::from_millis(500);
        let later = Duration::from_secs(5);
        // Just sent: still talking.
        assert_eq!(job_text(JobKind::Cancel, "running", "printing", soon), ("print.job_running", false));
        // Held behind the start macro: queued, not lost.
        assert_eq!(job_text(JobKind::Cancel, "running", "printing", later), ("print.cancel_queued", false));
        // The printer dropped the print: cancelled, whatever Moonraker answered.
        for answer in ["running", "done", "failed"] {
            assert_eq!(job_text(JobKind::Cancel, answer, "standby", later), ("print.cancel_done", false));
            assert_eq!(job_text(JobKind::Cancel, answer, "cancelled", later), ("print.cancel_done", false));
        }
        // A pause the same way; a resume is done once the printer prints again.
        assert_eq!(job_text(JobKind::Pause, "running", "printing", later), ("print.pause_queued", false));
        assert_eq!(job_text(JobKind::Pause, "failed", "paused", later), ("print.pause_done", false));
        assert_eq!(job_text(JobKind::Resume, "done", "printing", later), ("print.resume_done", false));
        assert_eq!(job_text(JobKind::Resume, "running", "paused", later), ("print.resume_queued", false));
        // Uploads and heats keep the host's own message; a failure is a warning.
        assert_eq!(job_text(JobKind::Upload, "done", "printing", later), ("", false));
        assert_eq!(job_text(JobKind::Upload, "failed", "printing", later), ("", true));
        assert_eq!(job_text(JobKind::Heat, "running", "standby", later), ("print.job_running", false));
    }
}
