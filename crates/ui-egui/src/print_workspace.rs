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
use solidcraft_engine::manufacture::{Feature, Layer, Toolpaths};
use solidcraft_engine::render::Camera;

use crate::gpu::{Frame, GpuScene, SceneSlot, ViewportCallback};
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
}

pub struct State {
    pub settings: Settings,
    settings_loaded: bool,
    design_camera: Option<(Camera, bool)>,
    /// The highest layer shown, as an index; `None` shows every layer.
    pub top_layer: Option<usize>,
    pub show_travels: bool,
    slot: SceneSlot,
    key: Option<u64>,
    /// What the uploaded scene was made from.
    uploaded: Option<Source>,
}

/// What the canvas shows: the held slice's toolpaths, or the design's bodies before any slice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    Toolpaths { slice: usize, travels: bool },
    Model { revision: u64, world: usize },
}

impl Default for State {
    fn default() -> Self {
        Self {
            settings: Settings::default(),
            settings_loaded: false,
            design_camera: None,
            top_layer: None,
            show_travels: true,
            slot: Arc::new(Mutex::new(None)),
            key: None,
            uploaded: None,
        }
    }
}

pub fn active(app: &SolveApp) -> bool {
    app.ui.tab == "PRINT"
}

/// The colour a feature's beads are drawn in (straight sRGB), one per feature, chosen for the
/// walls, surfaces and infill to tell apart at a glance in both themes.
pub fn feature_color(feature: Option<Feature>) -> [u8; 4] {
    match feature {
        Some(Feature::OuterWall) => [0xff, 0x5f, 0x1f, 255],
        Some(Feature::InnerWall) => [0xff, 0xd2, 0x1f, 255],
        Some(Feature::OverhangWall) => [0x1f, 0x6f, 0xff, 255],
        Some(Feature::TopSurface) => [0xff, 0x3b, 0x6b, 255],
        Some(Feature::BottomSurface) => [0xc9, 0x4b, 0xff, 255],
        Some(Feature::InternalSolid) => [0xb2, 0x3c, 0x3c, 255],
        Some(Feature::SparseInfill) => [0x3c, 0xb3, 0x71, 255],
        Some(Feature::Bridge) => [0x4a, 0xc0, 0xe0, 255],
        Some(Feature::InternalBridge) => [0x2f, 0x8e, 0xa8, 255],
        Some(Feature::Support) => [0xb4, 0xb4, 0xb4, 255],
        Some(Feature::SupportInterface) => [0x6e, 0x6e, 0x6e, 255],
        Some(Feature::Brim) => [0xe8, 0xc8, 0x9a, 255],
        Some(Feature::Skirt) => [0xa8, 0xd8, 0xa8, 255],
        Some(Feature::Ironing) => [0xff, 0x9a, 0xd5, 255],
        Some(Feature::GapFill) => [0xf4, 0xf4, 0xf4, 255],
        Some(Feature::PrimeTower) => [0xa0, 0xa0, 0xff, 255],
        Some(Feature::Custom) | None => [0xcc, 0xcc, 0x66, 255],
    }
}

const TRAVEL_COLOR: [u8; 4] = [0x40, 0x80, 0xff, 160];

/// Enter (`on`) or leave the workspace. Entering keeps the Design camera to restore on return.
pub fn enter(app: &mut SolveApp, on: bool) -> Result<(), String> {
    if on {
        if app.session.active_sketch.is_some() || app.tool.is_some() || app.dialog.is_some() || app.file_load.busy() {
            return Err("finish the current modelling operation before entering Print".into());
        }
        if crate::render_workspace::active(app) {
            crate::render_workspace::enter(app, false)?;
        }
        if !active(app) {
            app.print_workspace.design_camera = Some((app.cam, app.ui.perspective));
            app.ui.tab = "PRINT".into();
            if let Some(b) = bounds(app) {
                app.cam.fit(&b);
            }
        }
    } else if active(app) {
        if let Some((camera, perspective)) = app.print_workspace.design_camera.take() {
            app.cam = camera;
            app.ui.up_axis = camera.up;
            app.ui.perspective = perspective;
        }
        app.ui.tab = "MODEL".into();
    }
    app.cancel_view_animation();
    Ok(())
}

/// The workspace menu's choice: `design`, `render` or `print`.
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
    let settings = std::mem::take(&mut app.print_workspace.settings);
    let settings_loaded = app.print_workspace.settings_loaded;
    app.print_workspace = State { settings, settings_loaded, ..Default::default() };
    if active(app) {
        app.ui.tab = "MODEL".into();
    }
}

/// What the canvas frames: the toolpaths of the held slice, else the design's bodies.
pub fn bounds(app: &SolveApp) -> Option<Aabb3> {
    if let Some(held) = app.session.print_slice()
        && let Some(b) = held.sliced.toolpaths.bounds()
    {
        let mut a = Aabb3::default();
        a.add(Vec3::new(f64::from(b.min[0]), f64::from(b.min[1]), f64::from(b.min[2])));
        a.add(Vec3::new(f64::from(b.max[0]), f64::from(b.max[1]), f64::from(b.max[2])));
        return Some(a);
    }
    solidcraft_engine::product_scene::extract(&app.session).ok().map(|s| s.bounds).filter(|b| !b.is_empty())
}

/// A ribbon button or shortcut of a `print.*` command.
pub fn start(app: &mut SolveApp, id: &str) {
    match id {
        "print.slice" => slice(app),
        "print.save" => save(app),
        _ => {}
    }
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
            if let Some(b) = bounds(app) {
                app.cam.fit(&b);
            }
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

fn load_settings(app: &mut SolveApp) {
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

fn save_settings(app: &mut SolveApp) {
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
                    ui.heading(app.tr("print.slice_title"));
                    ui.label(app.tr("print.config_files"));
                    wrapped_small(ui, app.tr("print.config_help"));
                    let mut remove = None;
                    for (i, file) in app.print_workspace.settings.config_files.clone().iter().enumerate() {
                        ui.horizontal(|ui| {
                            let name = std::path::Path::new(file).file_name().and_then(|s| s.to_str()).unwrap_or(file);
                            ui.label(name).on_hover_text(file);
                            if crate::ui_kit::button(ui, app.tr("print.config_remove"), crate::ui_kit::Button::Small).clicked() {
                                remove = Some(i);
                            }
                        });
                    }
                    if let Some(i) = remove
                        && i < app.print_workspace.settings.config_files.len()
                    {
                        app.print_workspace.settings.config_files.remove(i);
                        save_settings(app);
                    }
                    if crate::ui_kit::button(ui, app.tr("print.config_add"), crate::ui_kit::Button::Normal).clicked() {
                        app.pick_file("JSON", &["json"], |app, path| {
                            if let Some(path) = path
                                && !app.print_workspace.settings.config_files.contains(&path)
                            {
                                app.print_workspace.settings.config_files.push(path);
                                save_settings(app);
                            }
                        });
                    }
                    ui.add_space(6.0);
                    if crate::ui_kit::button(ui, app.tr("cmd.print.slice"), crate::ui_kit::Button::Normal).clicked() {
                        slice(app);
                    }
                    ui.separator();
                    match app.session.print_slice().cloned() {
                        None => {
                            ui.label(app.tr("print.not_sliced"));
                        }
                        Some(held) => {
                            result_rows(app, ui, &held);
                            ui.separator();
                            toolpath_controls(app, ui, &held);
                        }
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
    if n > 0 {
        let mut shown = app.print_workspace.top_layer.map_or(n, |i| i + 1).clamp(1, n);
        ui.label(app.tr("print.layer"));
        let slider = crate::ui_kit::Slider { step: Some(1.0), format: |v| format!("{v:.0}"), show_value: true, integer: true };
        if crate::ui_kit::slider(ui, &mut shown, 1..=n, slider).changed() {
            app.print_workspace.top_layer = (shown < n).then_some(shown - 1);
        }
        if let Some(layer) = paths.layers.get(shown - 1) {
            ui.small(format!("z {:.2} mm · {:.2} mm · {} · {} segments", layer.z, layer.height, hms(f64::from(layer.time_s)), layer.segments()));
        }
    }
    let travels_label = app.tr("print.show_travels").to_string();
    crate::ui_kit::checkbox(ui, &travels_label, &mut app.print_workspace.show_travels);
    ui.label(app.tr("print.features"));
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
    crate::viewport::refresh_layout(app, ui.ctx().pixels_per_point());
    let response = ui.interact(rect, ui.id().with("print_preview"), Sense::click_and_drag());
    crate::navigation::handle(app, ui, rect, &response, crate::navigation::NavCaps::RENDER);
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
        let clip = match (app.session.print_slice(), app.print_workspace.top_layer) {
            (Some(held), Some(i)) => held.sliced.toolpaths.layers.get(i).map_or([0.0; 4], |l| [0.0, 0.0, 1.0, l.z + 1e-3]),
            _ => [0.0; 4],
        };
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
            clip,
            cap: [0.0; 4],
            images: Vec::new(),
            analysis: [0.0; 8],
            analysis_colors: [0.0; 36],
            access: None,
            outline: false,
            ao_radius: 0.0,
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
}

/// Build the scene for what the canvas shows, when that changed.
fn upload(app: &mut SolveApp) -> Result<(), String> {
    let source = match app.session.print_slice() {
        Some(held) => Source::Toolpaths { slice: Arc::as_ptr(held) as usize, travels: app.print_workspace.show_travels },
        None => Source::Model { revision: app.session.revision, world: Arc::as_ptr(&app.session.world_state()) as usize },
    };
    if app.print_workspace.uploaded == Some(source) {
        return Ok(());
    }
    let scene = match app.session.print_slice().cloned() {
        Some(held) => toolpath_scene(&held, app.print_workspace.show_travels)?,
        None => model_scene(app)?,
    };
    if let Ok(mut slot) = app.print_workspace.slot.lock() {
        *slot = Some(scene);
    }
    app.print_workspace.key = Some(app.print_workspace.key.map_or(1, |k| k.wrapping_add(1)));
    app.print_workspace.uploaded = Some(source);
    Ok(())
}

/// The design's bodies in a neutral grey, with the bed as a grid when one is known.
fn model_scene(app: &SolveApp) -> Result<GpuScene, String> {
    let scene = solidcraft_engine::product_scene::extract(&app.session).map_err(|e| e.to_string())?;
    let mut gpu = GpuScene::default();
    let grey = [0xb4, 0xba, 0xc4, 255];
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
                gpu.tri(p, n, grey);
            }
        }
    }
    Ok(gpu)
}

/// The toolpaths as beads: a flat ribbon per segment at the layer's top, with sides down to
/// the layer below while the budget allows, else lines.
fn toolpath_scene(held: &HeldSlice, travels: bool) -> Result<GpuScene, String> {
    let paths: &Toolpaths = &held.sliced.toolpaths;
    let segments = paths.segments();
    let travel_count: usize = paths.layers.iter().map(|l| l.paths.len().saturating_sub(1)).sum();
    let travel_bytes = if travels { travel_count * LINE_BYTES } else { 0 };
    let detail = if segments * 6 * TRI_BYTES + travel_bytes <= MAX_GEOMETRY_BYTES {
        Detail::Beads
    } else if segments * 2 * TRI_BYTES + travel_bytes <= MAX_GEOMETRY_BYTES {
        Detail::Ribbons
    } else if segments * LINE_BYTES + travel_bytes <= MAX_GEOMETRY_BYTES {
        Detail::Lines
    } else {
        return Err(format!("{segments} segments are past the preview's geometry budget"));
    };
    let mut gpu = GpuScene::default();
    bed(&mut gpu, held.sliced.bed);
    for layer in &paths.layers {
        layer_beads(&mut gpu, layer, detail);
        if travels {
            for (a, b) in layer.travels() {
                gpu.line([a[0], a[1], layer.z], [b[0], b[1], layer.z], TRAVEL_COLOR, 1.0, false);
            }
        }
    }
    Ok(gpu)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Detail {
    /// Top ribbon and two sides per segment (six triangles).
    Beads,
    /// Top ribbon only (two triangles).
    Ribbons,
    Lines,
}

fn layer_beads(gpu: &mut GpuScene, layer: &Layer, detail: Detail) {
    let top = layer.z;
    let bottom = (layer.z - layer.height).max(0.0);
    for path in &layer.paths {
        let c = feature_color(path.feature);
        let half = (path.width_mm * 0.5).max(0.05);
        for w in path.points.windows(2) {
            let (a, b) = (w[0], w[1]);
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let len = dx.hypot(dy);
            if detail == Detail::Lines || len < 1e-6 {
                if len >= 1e-6 {
                    gpu.line([a[0], a[1], top], [b[0], b[1], top], c, 1.5, false);
                }
                continue;
            }
            // The side offset, perpendicular to the segment.
            let (nx, ny) = (-dy / len * half, dx / len * half);
            let a_l = [a[0] + nx, a[1] + ny];
            let a_r = [a[0] - nx, a[1] - ny];
            let b_l = [b[0] + nx, b[1] + ny];
            let b_r = [b[0] - nx, b[1] - ny];
            let up = [0.0, 0.0, 1.0];
            quad(gpu, [a_l[0], a_l[1], top], [b_l[0], b_l[1], top], [b_r[0], b_r[1], top], [a_r[0], a_r[1], top], up, c);
            if detail == Detail::Beads {
                let n_l = [nx / half, ny / half, 0.0];
                let n_r = [-nx / half, -ny / half, 0.0];
                quad(gpu, [a_l[0], a_l[1], bottom], [b_l[0], b_l[1], bottom], [b_l[0], b_l[1], top], [a_l[0], a_l[1], top], n_l, c);
                quad(gpu, [a_r[0], a_r[1], top], [b_r[0], b_r[1], top], [b_r[0], b_r[1], bottom], [a_r[0], a_r[1], bottom], n_r, c);
            }
        }
    }
}

/// Two triangles for the quad `p0 p1 p2 p3`, wound so `n` is its outward normal.
fn quad(gpu: &mut GpuScene, p0: [f32; 3], p1: [f32; 3], p2: [f32; 3], p3: [f32; 3], n: [f32; 3], c: [u8; 4]) {
    for p in [p0, p1, p2, p0, p2, p3] {
        gpu.tri(p, n, c);
    }
}

/// The bed outline and a 10 mm grid under everything.
fn bed(gpu: &mut GpuScene, b: [f32; 4]) {
    let [x0, y0, x1, y1] = b;
    if !(x1 > x0 && y1 > y0) || !b.iter().all(|v| v.is_finite()) {
        return;
    }
    let t = Tokens::get();
    let line = |c: Color32| [c.r(), c.g(), c.b(), 255];
    let (minor, major) = (line(t.grid), line(t.grid_major));
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
