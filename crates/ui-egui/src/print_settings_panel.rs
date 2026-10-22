//! The print settings on the Slice tab: a curated set of slicer keys in groups, each showing the
//! value that applies (from the settings files or the design's own override) with a control to
//! change it. A change goes through `print.settings.set`, which saves it in the design; the
//! files stay as they are. Anything the slicer refused on the last slice shows under its field.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::{SolveApp, theme::Tokens};

/// What a field edits.
#[derive(Clone, Copy)]
pub enum Kind {
    /// A number with a unit, written as the files write it (`"0.16"`).
    Number { min: f64, max: f64, step: f64, decimals: usize, unit: &'static str },
    /// A whole number.
    Int { min: i64, max: i64, unit: &'static str },
    /// A percentage written as `"15%"`.
    Percent,
    /// `"1"` or `"0"`.
    Bool,
    /// One of the engine's names; each with its label's message id.
    Choice(&'static [(&'static str, &'static str)]),
}

pub struct Field {
    pub key: &'static str,
    /// Message id of the label.
    pub label: &'static str,
    pub kind: Kind,
}

pub struct Group {
    /// Message id of the heading.
    pub label: &'static str,
    pub fields: &'static [Field],
}

const INFILL_PATTERNS: &[(&str, &str)] = &[
    ("gyroid", "print.pattern_gyroid"),
    ("grid", "print.pattern_grid"),
    ("cubic", "print.pattern_cubic"),
    ("adaptivecubic", "print.pattern_adaptivecubic"),
    ("crosshatch", "print.pattern_crosshatch"),
    ("honeycomb", "print.pattern_honeycomb"),
    ("lightning", "print.pattern_lightning"),
    ("rectilinear", "print.pattern_rectilinear"),
    ("triangles", "print.pattern_triangles"),
    ("concentric", "print.pattern_concentric"),
];
const SUPPORT_TYPES: &[(&str, &str)] = &[
    ("normal(auto)", "print.support_normal_auto"),
    ("tree(auto)", "print.support_tree_auto"),
    ("normal(manual)", "print.support_normal_manual"),
    ("tree(manual)", "print.support_tree_manual"),
];
const BRIM_TYPES: &[(&str, &str)] = &[
    ("auto_brim", "print.brim_auto"),
    ("no_brim", "print.brim_none"),
    ("outer_only", "print.brim_outer"),
    ("inner_only", "print.brim_inner"),
    ("outer_and_inner", "print.brim_both"),
];

/// The curated settings, in the order the panel shows them.
pub const GROUPS: &[Group] = &[
    Group {
        label: "print.group_quality",
        fields: &[
            Field {
                key: "layer_height",
                label: "print.layer_height",
                kind: Kind::Number { min: 0.04, max: 1.0, step: 0.02, decimals: 2, unit: "mm" },
            },
            Field {
                key: "initial_layer_print_height",
                label: "print.first_layer_height",
                kind: Kind::Number { min: 0.04, max: 1.0, step: 0.02, decimals: 2, unit: "mm" },
            },
            Field { key: "line_width", label: "print.line_width", kind: Kind::Number { min: 0.1, max: 2.0, step: 0.02, decimals: 2, unit: "mm" } },
        ],
    },
    Group {
        label: "print.group_strength",
        fields: &[
            Field { key: "wall_loops", label: "print.wall_loops", kind: Kind::Int { min: 1, max: 20, unit: "" } },
            Field { key: "top_shell_layers", label: "print.top_layers", kind: Kind::Int { min: 0, max: 30, unit: "" } },
            Field { key: "bottom_shell_layers", label: "print.bottom_layers", kind: Kind::Int { min: 0, max: 30, unit: "" } },
            Field { key: "sparse_infill_density", label: "print.infill_density", kind: Kind::Percent },
            Field { key: "sparse_infill_pattern", label: "print.infill_pattern", kind: Kind::Choice(INFILL_PATTERNS) },
        ],
    },
    Group {
        label: "print.group_support",
        fields: &[
            Field { key: "enable_support", label: "print.enable_support", kind: Kind::Bool },
            Field { key: "support_type", label: "print.support_type", kind: Kind::Choice(SUPPORT_TYPES) },
            Field { key: "support_threshold_angle", label: "print.support_angle", kind: Kind::Int { min: 0, max: 90, unit: "°" } },
        ],
    },
    Group {
        label: "print.group_adhesion",
        fields: &[
            Field { key: "brim_type", label: "print.brim_type", kind: Kind::Choice(BRIM_TYPES) },
            Field { key: "brim_width", label: "print.brim_width", kind: Kind::Number { min: 0.0, max: 30.0, step: 0.5, decimals: 1, unit: "mm" } },
            Field { key: "skirt_loops", label: "print.skirt_loops", kind: Kind::Int { min: 0, max: 10, unit: "" } },
        ],
    },
    Group {
        label: "print.group_speed",
        fields: &[
            Field {
                key: "outer_wall_speed",
                label: "print.outer_wall_speed",
                kind: Kind::Number { min: 1.0, max: 600.0, step: 5.0, decimals: 0, unit: "mm/s" },
            },
            Field {
                key: "inner_wall_speed",
                label: "print.inner_wall_speed",
                kind: Kind::Number { min: 1.0, max: 600.0, step: 5.0, decimals: 0, unit: "mm/s" },
            },
            Field {
                key: "sparse_infill_speed",
                label: "print.infill_speed",
                kind: Kind::Number { min: 1.0, max: 600.0, step: 5.0, decimals: 0, unit: "mm/s" },
            },
            Field {
                key: "initial_layer_speed",
                label: "print.first_layer_speed",
                kind: Kind::Number { min: 1.0, max: 600.0, step: 5.0, decimals: 0, unit: "mm/s" },
            },
        ],
    },
    Group {
        label: "print.group_temperature",
        fields: &[
            Field { key: "nozzle_temperature", label: "print.nozzle_temperature", kind: Kind::Int { min: 0, max: 350, unit: "°C" } },
            Field {
                key: "nozzle_temperature_initial_layer",
                label: "print.nozzle_temperature_first",
                kind: Kind::Int { min: 0, max: 350, unit: "°C" },
            },
            Field { key: "hot_plate_temp", label: "print.bed_temperature", kind: Kind::Int { min: 0, max: 150, unit: "°C" } },
            Field { key: "hot_plate_temp_initial_layer", label: "print.bed_temperature_first", kind: Kind::Int { min: 0, max: 150, unit: "°C" } },
        ],
    },
];

/// The values the panel last read, and the drafts its fields edit.
#[derive(Default)]
pub struct Cache {
    /// The settings files and the session revision the values were read for.
    key: Option<(Vec<String>, u64)>,
    /// Key to (value, source) as `print.settings.get` reported.
    values: BTreeMap<String, (Value, String)>,
    numbers: BTreeMap<&'static str, f64>,
    ints: BTreeMap<&'static str, i64>,
}

/// The first scalar of a value: a per-filament list gives its first entry.
fn scalar(v: &Value) -> Option<&Value> {
    match v {
        Value::Array(items) => items.first(),
        v => Some(v),
    }
}

fn as_number(v: &Value) -> Option<f64> {
    match scalar(v)? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().trim_end_matches('%').trim().parse().ok(),
        _ => None,
    }
}

fn as_bool(v: &Value) -> bool {
    match scalar(v) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => matches!(s.trim(), "1" | "true" | "on"),
        Some(Value::Number(n)) => n.as_f64().is_some_and(|x| x != 0.0),
        _ => false,
    }
}

fn as_text(v: &Value) -> String {
    match scalar(v) {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

/// A value written the way the files write it; a key the file holds as a list is written as one.
fn write_back(current: Option<&Value>, text: String) -> Value {
    if matches!(current, Some(Value::Array(_))) { json!([text]) } else { Value::String(text) }
}

/// Read the values again when the files or the design changed.
fn refresh(app: &mut SolveApp) {
    let files = app.print_workspace.settings.config_files.clone();
    let key = (files.clone(), app.session.revision);
    if app.print_workspace.settings_panel.key.as_ref() == Some(&key) {
        return;
    }
    let keys: Vec<&str> = GROUPS.iter().flat_map(|g| g.fields.iter().map(|f| f.key)).collect();
    let mut values = BTreeMap::new();
    if let Ok(v) = app.run("print.settings.get", json!({"config": files, "keys": keys}))
        && let Some(map) = v["settings"].as_object()
    {
        for (k, entry) in map {
            values.insert(k.clone(), (entry["value"].clone(), entry["source"].as_str().unwrap_or("default").to_string()));
        }
    }
    let cache = &mut app.print_workspace.settings_panel;
    cache.key = Some(key);
    cache.numbers.clear();
    cache.ints.clear();
    for field in GROUPS.iter().flat_map(|g| g.fields.iter()) {
        let value = values.get(field.key).and_then(|(v, _)| as_number(v));
        match field.kind {
            Kind::Number { min, .. } => {
                cache.numbers.insert(field.key, value.unwrap_or(min));
            }
            Kind::Percent => {
                cache.numbers.insert(field.key, value.unwrap_or(0.0));
            }
            Kind::Int { min, .. } => {
                cache.ints.insert(field.key, value.map_or(min, |v| v.round() as i64));
            }
            _ => {}
        }
    }
    cache.values = values;
}

fn set(app: &mut SolveApp, key: &str, value: Value) {
    if let Err(e) = app.run("print.settings.set", json!({"set": {key: value}})) {
        app.set_status(e, true);
    }
}

/// The settings groups, with the slicer's complaints from the last slice under their fields.
pub fn rows(app: &mut SolveApp, ui: &mut egui::Ui) {
    refresh(app);
    let t = Tokens::get();
    let refused: BTreeMap<String, String> =
        app.session.print_slice().map(|h| h.sliced.report.refused.iter().map(|r| (r.key.clone(), r.reason.clone())).collect()).unwrap_or_default();
    for group in GROUPS {
        ui.add_space(4.0);
        ui.label(egui::RichText::new(app.tr(group.label)).strong());
        for field in group.fields {
            let (current, source) = app.print_workspace.settings_panel.values.get(field.key).cloned().unwrap_or((Value::Null, "default".into()));
            let overridden = source == "design";
            ui.horizontal(|ui| {
                let label = app.tr(field.label).to_string();
                ui.label(if overridden { egui::RichText::new(label).color(t.accent) } else { egui::RichText::new(label) });
                if overridden && crate::ui_kit::button(ui, "↺", crate::ui_kit::Button::Small).on_hover_text(app.tr("print.reset_setting")).clicked()
                {
                    set(app, field.key, Value::Null);
                }
            });
            match field.kind {
                Kind::Number { min, max, step, decimals, unit } => {
                    let mut draft = app.print_workspace.settings_panel.numbers.get(field.key).copied().unwrap_or(min);
                    if crate::ui_kit::number_field(ui, &mut draft, min..=max, step, unit).changed() {
                        app.print_workspace.settings_panel.numbers.insert(field.key, draft);
                        set(app, field.key, write_back(Some(&current), format!("{draft:.decimals$}")));
                    }
                }
                Kind::Percent => {
                    let mut draft = app.print_workspace.settings_panel.numbers.get(field.key).copied().unwrap_or(0.0);
                    if crate::ui_kit::number_field(ui, &mut draft, 0.0..=100.0, 1.0, "%").changed() {
                        app.print_workspace.settings_panel.numbers.insert(field.key, draft);
                        set(app, field.key, write_back(Some(&current), format!("{draft:.0}%")));
                    }
                }
                Kind::Int { min, max, unit } => {
                    let mut draft = app.print_workspace.settings_panel.ints.get(field.key).copied().unwrap_or(min);
                    if crate::ui_kit::number_field(ui, &mut draft, min..=max, 1.0, unit).changed() {
                        app.print_workspace.settings_panel.ints.insert(field.key, draft);
                        set(app, field.key, write_back(Some(&current), draft.to_string()));
                    }
                }
                Kind::Bool => {
                    let mut on = as_bool(&current);
                    let label = app.tr("print.enabled").to_string();
                    if crate::ui_kit::checkbox(ui, &label, &mut on).changed() {
                        set(app, field.key, write_back(Some(&current), if on { "1".into() } else { "0".into() }));
                    }
                }
                Kind::Choice(options) => {
                    let value = as_text(&current);
                    let shown = options.iter().find(|(v, _)| *v == value).map_or(value.clone(), |(_, l)| app.tr(l).to_string());
                    let mut picked: Option<&'static str> = None;
                    crate::ui_kit::dropdown_menu(ui, ("print_setting", field.key), &shown, |ui| {
                        for (option, label) in options {
                            if crate::ui_kit::list_picker(ui, option, app.tr(label), *option == value).clicked() {
                                picked = Some(option);
                                ui.close();
                            }
                        }
                    });
                    if let Some(option) = picked
                        && option != value
                    {
                        set(app, field.key, write_back(Some(&current), option.to_string()));
                    }
                }
            }
            if let Some(reason) = refused.get(field.key) {
                ui.colored_label(t.warning, reason);
            }
        }
    }
    if !app.session.doc.print.overlay.is_empty() {
        ui.add_space(4.0);
        ui.small(crate::i18n::format("print.overrides_count", &[("n", &app.session.doc.print.overlay.len().to_string())]));
        if crate::ui_kit::button(ui, app.tr("print.reset_all_settings"), crate::ui_kit::Button::Small).clicked()
            && let Err(e) = app.run("print.settings.clear", json!({}))
        {
            app.set_status(e, true);
        }
    }
}
