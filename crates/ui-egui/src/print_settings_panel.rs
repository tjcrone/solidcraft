//! The Print workspace's Settings panel, in the shared host's left column on Prepare and Preview: the
//! profile files the values come from, then the slicer's settings in the sections OrcaSlicer
//! users know (Printer, Filament, and Process as Quality, Strength, Speed, Support and Others),
//! each folded until opened, and an Advanced section for any key the catalogue leaves out. A
//! search above them shows the matching fields of every section. Every field shows the value
//! that applies (the design's own override, else the files', else the slicer's default) and
//! changes it at once through `print.settings.set`, which is undoable and saved in the design.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::{SolveApp, ui_kit};
use solidcraft_theme::Tokens;

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

/// One section of the catalogue.
pub struct Section {
    /// Message id of the section's name.
    pub label: &'static str,
    pub fields: &'static [Field],
}

const fn num(key: &'static str, label: &'static str, min: f64, max: f64, step: f64, decimals: usize, unit: &'static str) -> Field {
    Field { key, label, kind: Kind::Number { min, max, step, decimals, unit } }
}
const fn mm(key: &'static str, label: &'static str, min: f64, max: f64) -> Field {
    num(key, label, min, max, 0.02, 2, "mm")
}
const fn speed(key: &'static str, label: &'static str) -> Field {
    num(key, label, 1.0, 1000.0, 5.0, 0, "mm/s")
}
const fn int(key: &'static str, label: &'static str, min: i64, max: i64, unit: &'static str) -> Field {
    Field { key, label, kind: Kind::Int { min, max, unit } }
}
const fn pct(key: &'static str, label: &'static str) -> Field {
    Field { key, label, kind: Kind::Percent }
}
const fn flag(key: &'static str, label: &'static str) -> Field {
    Field { key, label, kind: Kind::Bool }
}
const fn pick(key: &'static str, label: &'static str, options: &'static [(&'static str, &'static str)]) -> Field {
    Field { key, label, kind: Kind::Choice(options) }
}

const INFILL_PATTERNS: &[(&str, &str)] = &[
    ("gyroid", "print.opt.gyroid"),
    ("grid", "print.opt.grid"),
    ("cubic", "print.opt.cubic"),
    ("adaptivecubic", "print.opt.adaptivecubic"),
    ("crosshatch", "print.opt.crosshatch"),
    ("honeycomb", "print.opt.honeycomb"),
    ("lightning", "print.opt.lightning"),
    ("rectilinear", "print.opt.rectilinear"),
    ("triangles", "print.opt.triangles"),
    ("concentric", "print.opt.concentric"),
];
/// The line and curve fills the slicer draws solid layers with (top, bottom and internal solid).
const SOLID_PATTERNS: &[(&str, &str)] = &[
    ("monotonic", "print.opt.monotonic"),
    ("monotonicline", "print.opt.monotonicline"),
    ("rectilinear", "print.opt.rectilinear"),
    ("alignedrectilinear", "print.opt.alignedrectilinear"),
    ("concentric", "print.opt.concentric"),
    ("hilbertcurve", "print.opt.hilbertcurve"),
    ("archimedeanchords", "print.opt.archimedeanchords"),
    ("octagramspiral", "print.opt.octagramspiral"),
];
const SUPPORT_TYPES: &[(&str, &str)] = &[
    ("normal(auto)", "print.opt.normal_auto"),
    ("tree(auto)", "print.opt.tree_auto"),
    ("normal(manual)", "print.opt.normal_manual"),
    ("tree(manual)", "print.opt.tree_manual"),
];
const SUPPORT_STYLES: &[(&str, &str)] = &[
    ("default", "print.opt.default"),
    ("grid", "print.opt.grid"),
    ("snug", "print.opt.snug"),
    ("tree_slim", "print.opt.tree_slim"),
    ("tree_strong", "print.opt.tree_strong"),
    ("tree_hybrid", "print.opt.tree_hybrid"),
    ("organic", "print.opt.organic"),
];
const SUPPORT_BASE_PATTERNS: &[(&str, &str)] = &[
    ("default", "print.opt.default"),
    ("rectilinear", "print.opt.rectilinear"),
    ("rectilinear-grid", "print.opt.rectilinear_grid"),
    ("honeycomb", "print.opt.honeycomb"),
    ("lightning", "print.opt.lightning"),
    ("hollow", "print.opt.hollow"),
];
const INTERFACE_PATTERNS: &[(&str, &str)] = &[
    ("auto", "print.opt.auto"),
    ("rectilinear", "print.opt.rectilinear"),
    ("concentric", "print.opt.concentric"),
    ("rectilinear_interlaced", "print.opt.rectilinear_interlaced"),
    ("grid", "print.opt.grid"),
];
const BRIM_TYPES: &[(&str, &str)] = &[
    ("auto_brim", "print.opt.auto"),
    ("no_brim", "print.opt.none"),
    ("outer_only", "print.opt.outer_only"),
    ("inner_only", "print.opt.inner_only"),
    ("outer_and_inner", "print.opt.outer_and_inner"),
];
const SEAM_POSITIONS: &[(&str, &str)] =
    &[("aligned", "print.opt.aligned"), ("nearest", "print.opt.nearest"), ("back", "print.opt.back"), ("random", "print.opt.random")];
const WALL_SEQUENCES: &[(&str, &str)] = &[
    ("inner wall/outer wall", "print.opt.inner_outer"),
    ("outer wall/inner wall", "print.opt.outer_inner"),
    ("inner-outer-inner wall", "print.opt.inner_outer_inner"),
];
const SHELL_THICKNESS: &[(&str, &str)] = &[
    ("none", "print.opt.none"),
    ("ensure_critical_only", "print.opt.critical_only"),
    ("ensure_moderate", "print.opt.moderate"),
    ("ensure_all", "print.opt.all"),
];
const FUZZY_SKIN: &[(&str, &str)] =
    &[("none", "print.opt.none"), ("external", "print.opt.outer_walls"), ("all", "print.opt.all_walls"), ("allwalls", "print.opt.all_walls_contour")];
const Z_HOP_TYPES: &[(&str, &str)] = &[
    ("Normal Lift", "print.opt.lift_normal"),
    ("Slope Lift", "print.opt.lift_slope"),
    ("Spiral Lift", "print.opt.lift_spiral"),
    ("Auto Lift", "print.opt.lift_auto"),
];
const PRINT_SEQUENCES: &[(&str, &str)] = &[("by layer", "print.opt.by_layer"), ("by object", "print.opt.by_object")];
const GCODE_FLAVORS: &[(&str, &str)] = &[
    ("klipper", "print.opt.klipper"),
    ("marlin", "print.opt.marlin"),
    ("marlin2", "print.opt.marlin2"),
    ("reprapfirmware", "print.opt.reprapfirmware"),
];

/// The catalogue, one section per page, in the order the window lists them. Every key is one
/// the slicer reads (a test checks), named as OrcaSlicer files name it.
pub const SECTIONS: &[Section] = &[
    Section {
        label: "print.page_printer",
        fields: &[
            num("nozzle_diameter", "print.key.nozzle_diameter", 0.1, 2.0, 0.05, 2, "mm"),
            num("printable_height", "print.key.printable_height", 10.0, 2000.0, 5.0, 0, "mm"),
            pick("gcode_flavor", "print.key.gcode_flavor", GCODE_FLAVORS),
            mm("retraction_length", "print.key.retraction_length", 0.0, 20.0),
            speed("retraction_speed", "print.key.retraction_speed"),
            speed("deretraction_speed", "print.key.deretraction_speed"),
            mm("retraction_minimum_travel", "print.key.retraction_minimum_travel", 0.0, 50.0),
            flag("retract_when_changing_layer", "print.key.retract_when_changing_layer"),
            mm("z_hop", "print.key.z_hop", 0.0, 10.0),
            pick("z_hop_types", "print.key.z_hop_types", Z_HOP_TYPES),
            mm("retract_lift_above", "print.key.retract_lift_above", 0.0, 2000.0),
            mm("retract_lift_below", "print.key.retract_lift_below", 0.0, 2000.0),
            flag("wipe", "print.key.wipe"),
            mm("wipe_distance", "print.key.wipe_distance", 0.0, 20.0),
            num("extruder_clearance_radius", "print.key.extruder_clearance_radius", 1.0, 500.0, 1.0, 0, "mm"),
            num("extruder_clearance_height_to_rod", "print.key.extruder_clearance_height_to_rod", 1.0, 500.0, 1.0, 0, "mm"),
            num("extruder_clearance_height_to_lid", "print.key.extruder_clearance_height_to_lid", 1.0, 1000.0, 1.0, 0, "mm"),
        ],
    },
    Section {
        label: "print.page_filament",
        fields: &[
            num("filament_diameter", "print.key.filament_diameter", 1.0, 3.5, 0.01, 2, "mm"),
            num("filament_flow_ratio", "print.key.filament_flow_ratio", 0.5, 1.5, 0.01, 3, ""),
            num("filament_density", "print.key.filament_density", 0.5, 5.0, 0.01, 2, "g/cm³"),
            num("filament_cost", "print.key.filament_cost", 0.0, 1000.0, 1.0, 2, "/kg"),
            int("nozzle_temperature", "print.key.nozzle_temperature", 0, 350, "°C"),
            int("nozzle_temperature_initial_layer", "print.key.nozzle_temperature_initial_layer", 0, 350, "°C"),
            int("hot_plate_temp", "print.key.hot_plate_temp", 0, 150, "°C"),
            int("hot_plate_temp_initial_layer", "print.key.hot_plate_temp_initial_layer", 0, 150, "°C"),
            flag("enable_pressure_advance", "print.key.enable_pressure_advance"),
            num("pressure_advance", "print.key.pressure_advance", 0.0, 2.0, 0.005, 3, ""),
            int("fan_min_speed", "print.key.fan_min_speed", 0, 100, "%"),
            int("fan_max_speed", "print.key.fan_max_speed", 0, 100, "%"),
            int("fan_cooling_layer_time", "print.key.fan_cooling_layer_time", 0, 1000, "s"),
            int("close_fan_the_first_x_layers", "print.key.close_fan_the_first_x_layers", 0, 1000, ""),
            flag("slow_down_for_layer_cooling", "print.key.slow_down_for_layer_cooling"),
            int("slow_down_layer_time", "print.key.slow_down_layer_time", 0, 1000, "s"),
            speed("slow_down_min_speed", "print.key.slow_down_min_speed"),
            flag("enable_overhang_bridge_fan", "print.key.enable_overhang_bridge_fan"),
            int("overhang_fan_speed", "print.key.overhang_fan_speed", 0, 100, "%"),
        ],
    },
    Section {
        label: "print.page_quality",
        fields: &[
            mm("layer_height", "print.key.layer_height", 0.04, 1.0),
            mm("initial_layer_print_height", "print.key.initial_layer_print_height", 0.04, 1.0),
            mm("line_width", "print.key.line_width", 0.1, 2.0),
            pick("seam_position", "print.key.seam_position", SEAM_POSITIONS),
            pick("wall_sequence", "print.key.wall_sequence", WALL_SEQUENCES),
            flag("only_one_wall_first_layer", "print.key.only_one_wall_first_layer"),
            pick("ensure_vertical_shell_thickness", "print.key.ensure_vertical_shell_thickness", SHELL_THICKNESS),
            pick("fuzzy_skin", "print.key.fuzzy_skin", FUZZY_SKIN),
            mm("fuzzy_skin_thickness", "print.key.fuzzy_skin_thickness", 0.0, 2.0),
            mm("fuzzy_skin_point_distance", "print.key.fuzzy_skin_point_distance", 0.0, 2.0),
            pct("bridge_flow", "print.key.bridge_flow"),
            flag("thick_bridges", "print.key.thick_bridges"),
            flag("small_area_infill_flow_compensation", "print.key.small_area_infill_flow_compensation"),
        ],
    },
    Section {
        label: "print.page_strength",
        fields: &[
            int("wall_loops", "print.key.wall_loops", 0, 50, ""),
            int("top_shell_layers", "print.key.top_shell_layers", 0, 50, ""),
            mm("top_shell_thickness", "print.key.top_shell_thickness", 0.0, 10.0),
            int("bottom_shell_layers", "print.key.bottom_shell_layers", 0, 50, ""),
            mm("bottom_shell_thickness", "print.key.bottom_shell_thickness", 0.0, 10.0),
            pct("sparse_infill_density", "print.key.sparse_infill_density"),
            pick("sparse_infill_pattern", "print.key.sparse_infill_pattern", INFILL_PATTERNS),
            pick("internal_solid_infill_pattern", "print.key.internal_solid_infill_pattern", SOLID_PATTERNS),
            pick("top_surface_pattern", "print.key.top_surface_pattern", SOLID_PATTERNS),
            pick("bottom_surface_pattern", "print.key.bottom_surface_pattern", SOLID_PATTERNS),
            pct("infill_wall_overlap", "print.key.infill_wall_overlap"),
            pct("top_bottom_infill_wall_overlap", "print.key.top_bottom_infill_wall_overlap"),
            mm("sparse_infill_anchor", "print.key.sparse_infill_anchor", 0.0, 50.0),
            mm("sparse_infill_anchor_max", "print.key.sparse_infill_anchor_max", 0.0, 100.0),
            pct("top_surface_density", "print.key.top_surface_density"),
            pct("bottom_surface_density", "print.key.bottom_surface_density"),
            flag("interface_shells", "print.key.interface_shells"),
        ],
    },
    Section {
        label: "print.page_speed",
        fields: &[
            speed("initial_layer_speed", "print.key.initial_layer_speed"),
            speed("initial_layer_infill_speed", "print.key.initial_layer_infill_speed"),
            speed("outer_wall_speed", "print.key.outer_wall_speed"),
            speed("inner_wall_speed", "print.key.inner_wall_speed"),
            speed("sparse_infill_speed", "print.key.sparse_infill_speed"),
            speed("internal_solid_infill_speed", "print.key.internal_solid_infill_speed"),
            speed("top_surface_speed", "print.key.top_surface_speed"),
            speed("gap_infill_speed", "print.key.gap_infill_speed"),
            speed("bridge_speed", "print.key.bridge_speed"),
            speed("internal_bridge_speed", "print.key.internal_bridge_speed"),
            speed("support_speed", "print.key.support_speed"),
            speed("support_interface_speed", "print.key.support_interface_speed"),
            speed("travel_speed", "print.key.travel_speed"),
            flag("enable_overhang_speed", "print.key.enable_overhang_speed"),
            speed("overhang_1_4_speed", "print.key.overhang_1_4_speed"),
            speed("overhang_2_4_speed", "print.key.overhang_2_4_speed"),
            speed("overhang_3_4_speed", "print.key.overhang_3_4_speed"),
            speed("overhang_4_4_speed", "print.key.overhang_4_4_speed"),
        ],
    },
    Section {
        label: "print.page_support",
        fields: &[
            flag("enable_support", "print.key.enable_support"),
            pick("support_type", "print.key.support_type", SUPPORT_TYPES),
            pick("support_style", "print.key.support_style", SUPPORT_STYLES),
            int("support_threshold_angle", "print.key.support_threshold_angle", 0, 90, "°"),
            flag("support_on_build_plate_only", "print.key.support_on_build_plate_only"),
            flag("support_critical_regions_only", "print.key.support_critical_regions_only"),
            flag("support_remove_small_overhang", "print.key.support_remove_small_overhang"),
            mm("support_top_z_distance", "print.key.support_top_z_distance", 0.0, 2.0),
            mm("support_bottom_z_distance", "print.key.support_bottom_z_distance", 0.0, 2.0),
            mm("support_object_xy_distance", "print.key.support_object_xy_distance", 0.0, 10.0),
            pick("support_base_pattern", "print.key.support_base_pattern", SUPPORT_BASE_PATTERNS),
            mm("support_base_pattern_spacing", "print.key.support_base_pattern_spacing", 0.0, 20.0),
            int("support_interface_top_layers", "print.key.support_interface_top_layers", 0, 20, ""),
            int("support_interface_bottom_layers", "print.key.support_interface_bottom_layers", -1, 20, ""),
            mm("support_interface_spacing", "print.key.support_interface_spacing", 0.0, 10.0),
            pick("support_interface_pattern", "print.key.support_interface_pattern", INTERFACE_PATTERNS),
            mm("support_expansion", "print.key.support_expansion", -10.0, 10.0),
            mm("support_line_width", "print.key.support_line_width", 0.0, 2.0),
            int("tree_support_branch_angle", "print.key.tree_support_branch_angle", 0, 60, "°"),
            mm("tree_support_branch_distance", "print.key.tree_support_branch_distance", 0.5, 20.0),
            mm("tree_support_branch_diameter", "print.key.tree_support_branch_diameter", 0.5, 20.0),
            int("tree_support_wall_count", "print.key.tree_support_wall_count", 0, 10, ""),
            int("raft_layers", "print.key.raft_layers", 0, 100, ""),
            mm("raft_contact_distance", "print.key.raft_contact_distance", 0.0, 2.0),
        ],
    },
    Section {
        label: "print.page_others",
        fields: &[
            pick("brim_type", "print.key.brim_type", BRIM_TYPES),
            num("brim_width", "print.key.brim_width", 0.0, 50.0, 0.5, 1, "mm"),
            int("skirt_loops", "print.key.skirt_loops", 0, 20, ""),
            num("skirt_distance", "print.key.skirt_distance", 0.0, 50.0, 0.5, 1, "mm"),
            int("skirt_height", "print.key.skirt_height", 0, 1000, ""),
            pick("print_sequence", "print.key.print_sequence", PRINT_SEQUENCES),
            flag("bridge_no_support", "print.key.bridge_no_support"),
        ],
    },
];

/// Every catalogued field.
pub fn fields() -> impl Iterator<Item = &'static Field> {
    SECTIONS.iter().flat_map(|s| s.fields.iter())
}

/// The panel's default width: room for a label, the widest control (a choice) and its reset button.
pub(crate) const WIDTH: f32 = 340.0;

/// The label column of the card's rows: what the width leaves beside a choice and its reset button.
const LABEL_WIDTH: f32 = 136.0;

/// The card and what it keeps between frames.
#[derive(Default)]
pub struct PrintSettingsPanel {
    /// The search text; while it is not empty the card shows the matching fields instead.
    pub filter: String,
    cache: Cache,
    /// The Advanced section's key and value being typed.
    custom_key: String,
    custom_value: String,
}

/// The values the card last read, and the drafts its fields edit.
#[derive(Default)]
struct Cache {
    /// The settings files and the session revision the values were read for.
    key: Option<(Vec<String>, u64)>,
    /// Key to (value, source, file) as `print.settings.get` reported.
    values: BTreeMap<String, (Value, String, String)>,
    numbers: BTreeMap<&'static str, f64>,
    ints: BTreeMap<&'static str, i64>,
}

/// How many settings the design overrides, beside the panel's title.
pub(crate) fn shared_status(app: &SolveApp) -> Option<String> {
    let overridden = app.session.doc.print.overlay.len();
    (overridden > 0).then(|| solidcraft_ui_text::format("print.overrides_short", &[("n", &overridden.to_string())]))
}

/// The panel's body in the shared host: the search, then the sections or the search's results.
pub(crate) fn shared_body(app: &mut SolveApp, ui: &mut egui::Ui) {
    crate::print_workspace::load_settings(app);
    ui_kit::scope(ui, |ui| {
        search_row(app, ui);
        let top = ui.cursor().top();
        let body = egui::ScrollArea::vertical().id_salt("print_settings").min_scrolled_height(0.0).auto_shrink([false, true]).show(ui, |ui| {
            if app.print_settings_panel.filter.trim().is_empty() {
                sections(app, ui);
            } else {
                search_results(app, ui);
            }
        });
        crate::panel_runtime::report_content_extent(ui, top, body.content_size.y, false);
    });
}

/// Every section, folded until opened; the profile files start open.
fn sections(app: &mut SolveApp, ui: &mut egui::Ui) {
    refresh(app);
    let refused = refused(app);
    let files = app.tr("print.page_files").to_string();
    ui_kit::section(ui, "print_settings:files", &files, true, |ui| files_section(app, ui));
    for section in SECTIONS {
        let title = app.tr(section.label).to_string();
        ui_kit::section(ui, ("print_settings", section.label), &title, false, |ui| {
            form(ui, |ui| {
                for field in section.fields {
                    field_row(app, ui, field, &refused);
                }
            });
        });
    }
    let advanced = app.tr("print.page_advanced").to_string();
    ui_kit::section(ui, "print_settings:advanced", &advanced, false, |ui| advanced_section(app, ui));
    ui.separator();
    let overridden = app.session.doc.print.overlay.len();
    let label = app.tr("print.reset_all_settings").to_string();
    if ui_kit::button(ui, &label, ui_kit::Button::Enabled(overridden > 0)).on_hover_text(app.tr("print.reset_all_hint")).clicked()
        && let Err(e) = app.run("print.settings.clear", json!({}))
    {
        app.set_status(e, true);
    }
}

fn search_row(app: &mut SolveApp, ui: &mut egui::Ui) {
    let id = ui.make_persistent_id("print_settings_search");
    let hint = app.tr("print.search").to_string();
    ui_kit::search_field(ui, id, &mut app.print_settings_panel.filter, &hint);
}

/// A field matches when its key or its label holds every word of the search.
fn matches(filter: &str, field: &Field) -> bool {
    let label = solidcraft_ui_text::tr(field.label).to_lowercase();
    filter.split_whitespace().all(|word| {
        let word = word.to_lowercase();
        field.key.contains(&word) || label.contains(&word)
    })
}

fn search_results(app: &mut SolveApp, ui: &mut egui::Ui) {
    refresh(app);
    let filter = app.print_settings_panel.filter.trim().to_string();
    let refused = refused(app);
    let mut any = false;
    for section in SECTIONS {
        let hits: Vec<&Field> = section.fields.iter().filter(|f| matches(&filter, f)).collect();
        if hits.is_empty() {
            continue;
        }
        any = true;
        let title = app.tr(section.label).to_string();
        ui_kit::settings_section(ui, section.label, &title, |ui| {
            form(ui, |ui| {
                for field in hits.iter().copied() {
                    field_row(app, ui, field, &refused);
                }
            });
        });
    }
    if !any {
        ui_kit::text(ui, app.tr("print.search_none"), ui_kit::Text::Hint("print_search"));
    }
}

fn form(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    ui_kit::settings_form(ui, LABEL_WIDTH, add);
}

/// What the slicer refused on the last slice, by key.
fn refused(app: &SolveApp) -> BTreeMap<String, String> {
    app.session.print_slice().map(|h| h.sliced.report.refused.iter().map(|r| (r.key.clone(), r.reason.clone())).collect()).unwrap_or_default()
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
    if app.print_settings_panel.cache.key.as_ref() == Some(&key) {
        return;
    }
    let keys: Vec<&str> = fields().map(|f| f.key).collect();
    let mut values = BTreeMap::new();
    if let Ok(v) = app.run("print.settings.get", json!({"config": files, "keys": keys}))
        && let Some(map) = v["settings"].as_object()
    {
        for (k, entry) in map {
            values.insert(
                k.clone(),
                (entry["value"].clone(), entry["source"].as_str().unwrap_or("default").to_string(), entry["file"].as_str().unwrap_or("").to_string()),
            );
        }
    }
    let cache = &mut app.print_settings_panel.cache;
    cache.key = Some(key);
    cache.numbers.clear();
    cache.ints.clear();
    for field in fields() {
        let value = values.get(field.key).and_then(|(v, _, _)| as_number(v));
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

/// Where a value comes from, for the control's hover text.
fn source_text(app: &SolveApp, source: &str, file: &str) -> String {
    match source {
        "design" => app.tr("print.source_design").to_string(),
        "file" => {
            let name = std::path::Path::new(file).file_name().and_then(|s| s.to_str()).unwrap_or(file);
            solidcraft_ui_text::format("print.source_file", &[("file", name)])
        }
        _ => app.tr("print.source_default").to_string(),
    }
}

/// One field: its label (marked when the design overrides it), the control, a button back to
/// the file's value, and the slicer's complaint from the last slice when it had one.
fn field_row(app: &mut SolveApp, ui: &mut egui::Ui, field: &'static Field, refused: &BTreeMap<String, String>) {
    let t = Tokens::get();
    let (current, source, file) =
        app.print_settings_panel.cache.values.get(field.key).cloned().unwrap_or((Value::Null, "default".into(), String::new()));
    let overridden = source == "design";
    // The row keeps its shape while a value is typed (the reset button is always there, enabled
    // only for an override): a row that changed under the field would take its focus.
    let label = app.tr(field.label).to_string();
    let hover = source_text(app, &source, &file);
    ui_kit::row(ui, &label, |ui| {
        let response = match field.kind {
            Kind::Number { min, max, step, decimals, unit } => {
                let mut draft = app.print_settings_panel.cache.numbers.get(field.key).copied().unwrap_or(min);
                let response = ui_kit::number_field_on_commit(ui, &mut draft, min..=max, step, unit);
                if response.changed() {
                    app.print_settings_panel.cache.numbers.insert(field.key, draft);
                    set(app, field.key, write_back(Some(&current), format!("{draft:.decimals$}")));
                }
                response
            }
            Kind::Percent => {
                let mut draft = app.print_settings_panel.cache.numbers.get(field.key).copied().unwrap_or(0.0);
                let response = ui_kit::number_field_on_commit(ui, &mut draft, 0.0..=1000.0, 1.0, "%");
                if response.changed() {
                    app.print_settings_panel.cache.numbers.insert(field.key, draft);
                    set(app, field.key, write_back(Some(&current), format!("{draft:.0}%")));
                }
                response
            }
            Kind::Int { min, max, unit } => {
                let mut draft = app.print_settings_panel.cache.ints.get(field.key).copied().unwrap_or(min);
                let response = ui_kit::number_field_on_commit(ui, &mut draft, min..=max, 1.0, unit);
                if response.changed() {
                    app.print_settings_panel.cache.ints.insert(field.key, draft);
                    set(app, field.key, write_back(Some(&current), draft.to_string()));
                }
                response
            }
            Kind::Bool => {
                let mut on = as_bool(&current);
                let text = app.tr("print.enabled").to_string();
                let response = ui_kit::checkbox(ui, &text, &mut on);
                if response.changed() {
                    set(app, field.key, write_back(Some(&current), if on { "1".into() } else { "0".into() }));
                }
                response
            }
            Kind::Choice(options) => {
                let value = as_text(&current);
                let shown = options.iter().find(|(v, _)| *v == value).map_or(value.clone(), |(_, l)| app.tr(l).to_string());
                let mut picked: Option<&'static str> = None;
                // A choice takes the width it is given: leave the reset button its room in the card.
                let width = (ui.available_width() - ui.spacing().interact_size.y - 2.0 * ui.spacing().item_spacing.x).max(t.control_height);
                let response = ui
                    .scope(|ui| {
                        ui.set_max_width(width);
                        ui_kit::dropdown_menu(ui, ("print_setting", field.key), &shown, |ui| {
                            for (option, label) in options {
                                if ui_kit::list_picker(ui, option, solidcraft_ui_text::tr(label), *option == value).clicked() {
                                    picked = Some(option);
                                    ui.close();
                                }
                            }
                        })
                    })
                    .inner;
                if let Some(option) = picked
                    && option != value
                {
                    set(app, field.key, write_back(Some(&current), option.to_string()));
                }
                response
            }
        };
        response.on_hover_text(&hover);
        let reset = ui.add_enabled_ui(overridden, |ui| ui_kit::button(ui, "↺", ui_kit::Button::Small)).inner;
        if reset.on_hover_text(app.tr("print.reset_setting")).clicked() {
            set(app, field.key, Value::Null);
        }
    });
    if let Some(reason) = refused.get(field.key) {
        ui_kit::status_line(ui, reason, ui_kit::StatusTone::Warning);
    }
}

/// The profile files the values come from: OrcaSlicer-style JSON, one each for the printer,
/// the filament and the process.
fn files_section(app: &mut SolveApp, ui: &mut egui::Ui) {
    ui_kit::text(ui, app.tr("print.config_help"), ui_kit::Text::Hint("print_files_help"));
    let mut remove = None;
    for (i, file) in app.print_workspace.settings.config_files.clone().iter().enumerate() {
        ui.horizontal(|ui| {
            let name = std::path::Path::new(file).file_name().and_then(|s| s.to_str()).unwrap_or(file);
            ui.label(name).on_hover_text(file);
            if ui_kit::button(ui, app.tr("print.config_remove"), ui_kit::Button::Small).clicked() {
                remove = Some(i);
            }
        });
    }
    if app.print_workspace.settings.config_files.is_empty() {
        ui_kit::text(ui, app.tr("print.config_none"), ui_kit::Text::Hint("print_files"));
    }
    if let Some(i) = remove
        && i < app.print_workspace.settings.config_files.len()
    {
        app.print_workspace.settings.config_files.remove(i);
        crate::print_workspace::save_settings(app);
    }
    if ui_kit::button(ui, app.tr("print.config_add"), ui_kit::Button::Normal).clicked() {
        app.pick_file("JSON", &["json"], |app, path| {
            if let Some(path) = path
                && !app.print_workspace.settings.config_files.contains(&path)
            {
                app.print_workspace.settings.config_files.push(path);
                crate::print_workspace::save_settings(app);
            }
        });
    }
}

/// Every override the design holds, each with its way back, and a row to set any key.
fn advanced_section(app: &mut SolveApp, ui: &mut egui::Ui) {
    let t = Tokens::get();
    ui_kit::text(ui, app.tr("print.advanced_hint"), ui_kit::Text::Hint("print_advanced_help"));
    let overlay = app.session.doc.print.overlay.clone();
    if overlay.is_empty() {
        ui_kit::text(ui, app.tr("print.no_overrides"), ui_kit::Text::Hint("print_overrides"));
    }
    form(ui, |ui| {
        for (key, value) in &overlay {
            let shown = match value {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            ui_kit::row(ui, key, |ui| {
                ui.label(egui::RichText::new(shown).monospace());
                if ui_kit::button(ui, "↺", ui_kit::Button::Small).on_hover_text(app.tr("print.reset_setting")).clicked() {
                    set(app, key, Value::Null);
                }
            });
        }
    });
    ui.add_space(t.space_8);
    let key_label = app.tr("print.advanced_key").to_string();
    let value_label = app.tr("print.advanced_value").to_string();
    form(ui, |ui| {
        ui_kit::row(ui, &key_label, |ui| {
            let id = ui.make_persistent_id("print_custom_key");
            let hint = app.tr("print.key_example").to_string();
            ui_kit::bounded_cell_editor(ui, id, &mut app.print_settings_panel.custom_key, &hint, false, 64);
        });
        ui_kit::row(ui, &value_label, |ui| {
            let id = ui.make_persistent_id("print_custom_value");
            let hint = app.tr("print.value_example").to_string();
            ui_kit::bounded_cell_editor(ui, id, &mut app.print_settings_panel.custom_value, &hint, false, 256);
        });
    });
    let key = app.print_settings_panel.custom_key.trim().to_string();
    let value = app.print_settings_panel.custom_value.trim().to_string();
    let known = solidcraft_engine::print::engine_reads(&key);
    if !key.is_empty() && !known {
        ui_kit::status_line(ui, app.tr("print.advanced_unknown"), ui_kit::StatusTone::Warning);
    }
    if ui_kit::button(ui, app.tr("print.advanced_set"), ui_kit::Button::Enabled(!key.is_empty() && !value.is_empty())).clicked() {
        set(app, &key, Value::String(value));
        app.print_settings_panel.custom_key.clear();
        app.print_settings_panel.custom_value.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_catalogued_key_is_one_the_slicer_reads() {
        let mut seen = std::collections::BTreeSet::new();
        for field in fields() {
            assert!(solidcraft_engine::print::engine_reads(field.key), "the slicer never reads `{}`", field.key);
            assert!(seen.insert(field.key), "`{}` is catalogued twice", field.key);
            assert!(solidcraft_engine::doc::print_settings::PrintSettings::validate_key(field.key).is_ok(), "{}", field.key);
        }
        assert!(seen.len() >= 100, "{} keys", seen.len());
    }

    #[test]
    fn every_label_and_option_is_translated() {
        let mut ids: Vec<&str> = fields().map(|f| f.label).collect();
        ids.extend(SECTIONS.iter().map(|s| s.label));
        ids.extend(["print.settings_title", "print.overrides_short", "print.page_files", "print.page_advanced"]);
        for field in fields() {
            if let Kind::Choice(options) = field.kind {
                ids.extend(options.iter().map(|(_, l)| *l));
            }
        }
        for id in ids {
            assert_ne!(solidcraft_ui_text::tr(id), id, "no English text for `{id}`");
        }
    }

    #[test]
    fn choices_name_the_slicers_values() {
        for field in fields() {
            if let Kind::Choice(options) = field.kind {
                let mut values = std::collections::BTreeSet::new();
                for (value, _) in options {
                    assert!(!value.is_empty() && values.insert(*value), "{}: `{value}`", field.key);
                }
            }
        }
    }

    #[test]
    fn search_matches_key_and_label_words() {
        let field = fields().find(|f| f.key == "sparse_infill_density").unwrap_or_else(|| panic!("catalogued"));
        assert!(matches("infill", field));
        assert!(matches("sparse density", field));
        assert!(!matches("support", field));
    }
}
