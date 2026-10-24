//! The Printer tab of the Print workspace: the camera stream fills the canvas, the panel shows
//! what the printer is doing with its temperatures and the pause, resume and cancel buttons.
//! Everything that changes the printer goes through a `print.*` command; the frames of the
//! camera come straight from the installed printer host, which reads the stream off-thread.

use egui::{Color32, Sense, vec2};
use serde_json::json;

use crate::{
    SolveApp,
    print_workspace::{Job, JobKind},
    theme::Tokens,
};

#[derive(Default)]
pub struct State {
    /// The stream the host is asked to watch, so a change in the camera list restarts it.
    watching: Option<String>,
    /// The frame on the GPU and the sequence number it came from.
    texture: Option<(u64, egui::TextureHandle)>,
    /// The newest frame's size, pixels.
    size: Option<[usize; 2]>,
    /// The temperatures typed into the fields.
    pub nozzle_target: f64,
    pub bed_target: f64,
    fields_seeded: bool,
}

pub fn active(app: &SolveApp) -> bool {
    app.ui.tab == "PRINTER"
}

/// A ribbon button or shortcut of a printer command.
pub fn start(app: &mut SolveApp, id: &str) {
    let kind = match id {
        "print.pause" => JobKind::Pause,
        "print.resume" => JobKind::Resume,
        "print.cancel" => JobKind::Cancel,
        _ => return,
    };
    let result = app.run(id, json!({}));
    follow(app, result, kind);
}

fn follow(app: &mut SolveApp, result: Result<serde_json::Value, String>, kind: JobKind) {
    match result {
        Ok(v) => {
            let id = v["job"].as_u64().or_else(|| v["jobs"].as_array().and_then(|j| j.last()).and_then(|j| j.as_u64()));
            app.print_workspace.job = id.map(|id| Job::new(id, kind));
        }
        Err(e) => app.set_status(e, true),
    }
}

/// Keep the camera running while the tab shows, and stop it when it does not.
pub fn sync_camera(app: &mut SolveApp) {
    let wanted =
        if active(app) { app.session.printer_host().and_then(|h| h.webcams().into_iter().find(|w| w.enabled).map(|w| w.stream_url)) } else { None };
    if app.printer_dashboard.watching == wanted {
        return;
    }
    if let Some(host) = app.session.printer_host() {
        match &wanted {
            Some(url) => host.watch_camera(url, true),
            None => {
                if let Some(old) = &app.printer_dashboard.watching {
                    host.watch_camera(old, false);
                }
            }
        }
    }
    app.printer_dashboard.watching = wanted;
    app.printer_dashboard.texture = None;
    app.printer_dashboard.size = None;
}

/// The canvas: the newest camera frame, letterboxed, or why there is none.
pub fn show(app: &mut SolveApp, ui: &mut egui::Ui) {
    let rect = ui.max_rect();
    app.viewport.rect = Some(rect);
    let t = Tokens::get();
    ui.painter().rect_filled(rect, 0.0, t.viewport_bottom);
    ui.interact(rect, ui.id().with("printer_dashboard"), Sense::hover());
    sync_camera(app);
    let message = match camera_frame(app, ui.ctx()) {
        Ok(true) => None,
        Ok(false) if app.printer_dashboard.watching.is_none() => Some(app.tr("printer.no_camera").to_string()),
        Ok(false) => Some(app.tr("printer.camera_waiting").to_string()),
        Err(e) => Some(crate::i18n::format("printer.camera_error", &[("error", &e)])),
    };
    if let Some((_, texture)) = &app.printer_dashboard.texture
        && let Some(size) = app.printer_dashboard.size
    {
        let free = crate::viewport_layout::free_rect(crate::shell::content_rect(app, rect), &crate::shell::covered_rects(app)).shrink(12.0);
        let aspect = size[0] as f32 / size[1].max(1) as f32;
        let fit = if free.width() / free.height() > aspect {
            vec2(free.height() * aspect, free.height())
        } else {
            vec2(free.width(), free.width() / aspect)
        };
        let image = egui::Rect::from_center_size(free.center(), fit);
        ui.painter().image(texture.id(), image, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
    }
    if let Some(text) = message {
        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(t.font_body), t.text_dim);
    }
    // Frames arrive on their own clock; look again soon.
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(66));
}

/// Upload the newest frame when it is new. `Ok(true)` when a frame is on the GPU.
fn camera_frame(app: &mut SolveApp, ctx: &egui::Context) -> Result<bool, String> {
    let Some(host) = app.session.printer_host() else { return Ok(false) };
    let Some(frame) = host.camera_frame()? else { return Ok(app.printer_dashboard.texture.is_some()) };
    if app.printer_dashboard.texture.as_ref().is_some_and(|(seq, _)| *seq == frame.sequence) {
        return Ok(true);
    }
    let decoded = image::load_from_memory_with_format(&frame.jpeg, image::ImageFormat::Jpeg).map_err(|e| e.to_string())?.to_rgba8();
    let (w, h) = (decoded.width() as usize, decoded.height() as usize);
    if w == 0 || h == 0 || w > 8192 || h > 8192 {
        return Err(format!("a {w} by {h} frame"));
    }
    let image = egui::ColorImage::from_rgba_unmultiplied([w, h], decoded.as_raw());
    match &mut app.printer_dashboard.texture {
        Some((seq, texture)) => {
            texture.set(image, egui::TextureOptions::LINEAR);
            *seq = frame.sequence;
        }
        None => app.printer_dashboard.texture = Some((frame.sequence, ctx.load_texture("sc_printer_camera", image, egui::TextureOptions::LINEAR))),
    }
    app.printer_dashboard.size = Some([w, h]);
    Ok(true)
}

/// The panel: the printer's state and temperatures, and the controls.
pub fn panel(app: &mut SolveApp, ui: &mut egui::Ui) {
    use crate::ui_kit::{Container, Edge, Surface};
    Container::new(ui.ctx(), Surface::Panel)
        .edge("sc_printer_dashboard", Edge::Right)
        .exact_size(300.0)
        .resizable(false)
        .frame(Container::new(ui.ctx(), Surface::Panel).edge_frame(Tokens::get().panel).inner_margin(10))
        .show(ui, |ui| {
            crate::ui_kit::scope(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.heading(app.tr("print.printer_title"));
                    rows(app, ui);
                    ui.separator();
                    if crate::ui_kit::button(ui, app.tr("printer.back_to_print"), crate::ui_kit::Button::Normal).clicked() {
                        app.ui.tab = "SLICE".into();
                    }
                });
            });
        });
}

fn rows(app: &mut SolveApp, ui: &mut egui::Ui) {
    let t = Tokens::get();
    let status = match app.run("print.status", json!({})) {
        Ok(v) => v,
        Err(e) => {
            ui.label(e);
            return;
        }
    };
    if status["installed"] != true {
        ui.label(app.tr("print.printer_none"));
        ui.small(app.tr("printer.set_address_hint"));
        return;
    }
    let state = status["state"].as_str().unwrap_or("").to_string();
    let reachable = status["reachable"] == true;
    if reachable {
        ui.label(crate::print_workspace::state_label(app, &state));
        let name = status["filename"].as_str().unwrap_or("");
        if !name.is_empty() && matches!(state.as_str(), "printing" | "paused") {
            ui.small(name);
            let progress = status["progress"].as_f64().unwrap_or(0.0);
            crate::ui_kit::progress(ui, "printer", progress.clamp(0.0, 1.0) as f32);
            let elapsed = status["print_duration_s"].as_f64().unwrap_or(0.0);
            let remaining = if progress > 0.01 { Some(elapsed / progress - elapsed) } else { None };
            ui.small(match remaining {
                Some(r) => format!(
                    "{:.0} % · {} · {}",
                    progress * 100.0,
                    crate::print_workspace::hms(elapsed),
                    crate::i18n::format("printer.remaining", &[("time", &crate::print_workspace::hms(r))])
                ),
                None => format!("{:.0} % · {}", progress * 100.0, crate::print_workspace::hms(elapsed)),
            });
        }
    } else {
        let text = match status["age_s"].as_f64() {
            Some(a) => crate::i18n::format("print.unreachable_since", &[("time", &crate::print_workspace::hms(a))]),
            None => app.tr("print.unreachable").to_string(),
        };
        ui.colored_label(t.warning, text);
        if let Some(e) = status["error"].as_str() {
            ui.add(egui::Label::new(egui::RichText::new(e).small()).wrap());
        }
    }
    ui.separator();
    ui.label(app.tr("printer.temperatures"));
    let nozzle = (status["extruder"]["temperature"].as_f64().unwrap_or(0.0), status["extruder"]["target"].as_f64().unwrap_or(0.0));
    let bed = (status["bed"]["temperature"].as_f64().unwrap_or(0.0), status["bed"]["target"].as_f64().unwrap_or(0.0));
    if !app.printer_dashboard.fields_seeded && reachable {
        app.printer_dashboard.nozzle_target = nozzle.1;
        app.printer_dashboard.bed_target = bed.1;
        app.printer_dashboard.fields_seeded = true;
    }
    heater_row(app, ui, "print.extruder", nozzle, true);
    heater_row(app, ui, "print.bed", bed, false);
    ui.separator();
    let busy = matches!(state.as_str(), "printing" | "paused");
    ui.horizontal(|ui| {
        if crate::ui_kit::button(ui, app.tr("cmd.print.pause"), crate::ui_kit::Button::Enabled(reachable && state == "printing")).clicked() {
            start(app, "print.pause");
        }
        if crate::ui_kit::button(ui, app.tr("cmd.print.resume"), crate::ui_kit::Button::Enabled(reachable && state == "paused")).clicked() {
            start(app, "print.resume");
        }
    });
    if crate::ui_kit::button(ui, app.tr("cmd.print.cancel"), crate::ui_kit::Button::Enabled(reachable && busy)).clicked() {
        start(app, "print.cancel");
    }
    crate::print_workspace::job_line(app, ui, &state);
    ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
}

/// One heater: the reading, the target, a field to type a new target and a button to send it.
fn heater_row(app: &mut SolveApp, ui: &mut egui::Ui, label: &str, (temperature, target): (f64, f64), nozzle: bool) {
    let most = if nozzle { 350.0 } else { 150.0 };
    ui.horizontal(|ui| {
        ui.label(app.tr(label));
        ui.small(format!("{temperature:.0} / {target:.0} °C"));
    });
    ui.horizontal(|ui| {
        let value = if nozzle { &mut app.printer_dashboard.nozzle_target } else { &mut app.printer_dashboard.bed_target };
        crate::ui_kit::number_field(ui, value, 0.0..=most, 5.0, "°C");
        let wanted = *value;
        if crate::ui_kit::button(ui, app.tr("printer.set"), crate::ui_kit::Button::Normal).clicked() {
            let params = if nozzle { json!({ "nozzle": wanted }) } else { json!({ "bed": wanted }) };
            let result = app.run("print.heat", params);
            follow(app, result, JobKind::Heat);
        }
        if crate::ui_kit::button(ui, app.tr("printer.off"), crate::ui_kit::Button::Enabled(target > 0.0)).clicked() {
            let params = if nozzle { json!({ "nozzle": 0 }) } else { json!({ "bed": 0 }) };
            let result = app.run("print.heat", params);
            follow(app, result, JobKind::Heat);
            if nozzle {
                app.printer_dashboard.nozzle_target = 0.0;
            } else {
                app.printer_dashboard.bed_target = 0.0;
            }
        }
    });
}
