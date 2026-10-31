use super::*;

/// A closed cube of side `s` with one corner at the origin, triangles counter-clockwise from outside.
fn cube(s: f32) -> Part {
    let positions = vec![[0., 0., 0.], [s, 0., 0.], [s, s, 0.], [0., s, 0.], [0., 0., s], [s, 0., s], [s, s, s], [0., s, s]];
    let triangles = vec![
        [0, 2, 1],
        [0, 3, 2], // bottom
        [4, 5, 6],
        [4, 6, 7], // top
        [0, 1, 5],
        [0, 5, 4], // front, -Y
        [1, 2, 6],
        [1, 6, 5], // right, +X
        [2, 3, 7],
        [2, 7, 6], // back, +Y
        [3, 0, 4],
        [3, 4, 7], // left, -X
    ];
    Part { name: "cube".into(), positions, triangles }
}

#[test]
fn a_cube_slices_to_klipper_gcode_with_one_layer_per_layer_height() {
    let parts = [cube(20.0)];
    let job = Job {
        parts: &parts,
        config: r#"{"layer_height": 0.2, "initial_layer_print_height": 0.2, "gcode_flavor": "klipper"}"#,
        placement: Default::default(),
        now: None,
    };
    let sliced = default_slicer().slice(&job).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(sliced.layers, 100);
    let text = String::from_utf8_lossy(&sliced.gcode);
    assert!(text.lines().any(|l| l.starts_with("G1 ") && l.contains('E')), "no extruding move in the G-code");
    assert!(sliced.time_s > 60.0, "print time {} s", sliced.time_s);
    assert!(sliced.filament_mm > 100.0, "filament {} mm", sliced.filament_mm);
    assert!(sliced.filament_g > 1.0, "filament {} g", sliced.filament_g);
    assert!(default_slicer().name().starts_with("SlicerX sx-core "));
}

#[test]
fn hostile_parts_and_settings_are_refused_without_a_panic() {
    let mut broken = cube(10.0);
    broken.triangles.push([0, 1, 99]);
    let nan = Part { name: "nan".into(), positions: vec![[f32::NAN; 3]; 3], triangles: vec![[0, 1, 2]] };
    for (parts, config) in [(vec![broken], "{}"), (vec![nan], "{}"), (vec![], "{}"), (vec![cube(10.0)], "not json"), (vec![cube(10.0)], "[1, 2]")] {
        let job = Job { parts: &parts, config, placement: Default::default(), now: None };
        assert!(default_slicer().slice(&job).is_err(), "{config} with {} parts", parts.len());
    }
}

#[test]
fn a_flat_part_has_nothing_to_print() {
    let flat = Part { name: "flat".into(), positions: vec![[0., 0., 0.], [10., 0., 0.], [0., 10., 0.]], triangles: vec![[0, 1, 2]] };
    let parts = [flat];
    let job = Job { parts: &parts, config: "{}", placement: Default::default(), now: None };
    assert!(default_slicer().slice(&job).is_err());
}

#[test]
fn a_value_the_engine_cannot_use_keeps_its_default_and_is_reported_with_the_keys_it_ignores() {
    let parts = [cube(20.0)];
    let config = r#"{"layer_height": 0.2, "initial_layer_print_height": 0.2, "support_object_xy_distance": "50%", "no_such_setting": 1}"#;
    let sliced = default_slicer().slice(&Job { parts: &parts, config, placement: Default::default(), now: None }).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(sliced.layers, 100);
    let refused: Vec<&str> = sliced.report.refused.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(refused, ["support_object_xy_distance"], "{:?}", sliced.report);
    assert_eq!(sliced.report.refused[0].value, "\"50%\"");
    assert_eq!(sliced.report.ignored, ["no_such_setting"], "{:?}", sliced.report);
}

#[test]
fn a_part_that_would_leave_the_printers_volume_is_refused_and_nothing_comes_back() {
    // The default bed is 256 mm square and 250 mm tall.
    let wide = Part { name: "wide".into(), ..cube(20.0) };
    let mut wide = wide;
    for p in &mut wide.positions {
        p[0] *= 20.0; // 400 mm in X
    }
    let mut tall = cube(20.0);
    for p in &mut tall.positions {
        p[2] *= 15.0; // 300 mm in Z
    }
    for part in [wide, tall] {
        let parts = [part];
        let r = default_slicer().slice(&Job { parts: &parts, config: "{}", placement: Default::default(), now: None });
        assert!(matches!(r, Err(Error::Unsafe(_))), "{r:?}");
    }
}

#[test]
fn a_template_that_sends_the_head_outside_the_volume_is_refused() {
    let parts = [cube(20.0)];
    for (key, line) in [
        ("machine_end_gcode", "M104 S0\\nM140 S0\\nG1 Z-2 F600"),
        ("machine_start_gcode", "G0 X400 Y10 Z5"),
        ("machine_end_gcode", "M104 S0\\nM140 S0\\nG1 Z600"),
    ] {
        let config = format!(r#"{{"{key}": "{line}"}}"#);
        let r = default_slicer().slice(&Job { parts: &parts, config: &config, placement: Default::default(), now: None });
        assert!(matches!(r, Err(Error::Unsafe(_))), "{key}: {line}: {r:?}");
    }
    // The same templates kept inside the volume slice fine.
    let config = r#"{"machine_start_gcode": "G28\nG0 X10 Y10 Z5", "machine_end_gcode": "M104 S0\nM140 S0\nG91\nG1 Z2\nG90\nG0 X0 Y250"}"#;
    let r = default_slicer().slice(&Job { parts: &parts, config, placement: Default::default(), now: None });
    assert!(r.is_ok(), "{:?}", r.err());
}

#[test]
fn a_purge_line_in_the_start_template_comes_back_as_a_file_bead() {
    let parts = [cube(20.0)];
    // A purge across the bed at 0.3 mm, then a lift and a retraction, which are not beads.
    let config = r#"{"machine_start_gcode": "G28\nM83\nG90\nG0 X10 Y10 Z0.3 F6000\nG1 X110 E5 F1200\nG1 E-2 F1800\nG0 Z5"}"#;
    let sliced = default_slicer().slice(&Job { parts: &parts, config, placement: Default::default(), now: None }).unwrap_or_else(|e| panic!("{e}"));
    let beads = &sliced.toolpaths.file_beads;
    assert_eq!(beads.len(), 1, "{beads:?}");
    assert_eq!(beads[0].points, vec![[10.0, 10.0, 0.3], [110.0, 10.0, 0.3]]);
    assert!((beads[0].length_mm() - 100.0).abs() < 1e-3);
    assert!(beads[0].width_mm > 0.3 && beads[0].width_mm < 0.5, "{}", beads[0].width_mm);
    // The default templates command no bead.
    let plain =
        default_slicer().slice(&Job { parts: &parts, config: "{}", placement: Default::default(), now: None }).unwrap_or_else(|e| panic!("{e}"));
    assert!(plain.toolpaths.file_beads.is_empty());
}

#[test]
fn the_toolpaths_come_back_in_millimetres_with_one_layer_per_layer_and_outer_walls_in_each() {
    let parts = [cube(20.0)];
    let job = Job {
        parts: &parts,
        config: r#"{"layer_height": 0.2, "initial_layer_print_height": 0.2, "gcode_flavor": "klipper"}"#,
        placement: Default::default(),
        now: None,
    };
    let sliced = default_slicer().slice(&job).unwrap_or_else(|e| panic!("{e}"));
    let paths = &sliced.toolpaths;
    assert_eq!(paths.layers.len(), 100);
    assert!(paths.segments() > 1000, "{} segments", paths.segments());
    // Layer tops climb by the layer height; the first layer is the first layer height tall.
    assert!((paths.layers[0].z - 0.2).abs() < 1e-4, "first layer top {}", paths.layers[0].z);
    assert!((paths.layers[99].z - 20.0).abs() < 1e-3, "last layer top {}", paths.layers[99].z);
    assert!(paths.layers.windows(2).all(|w| w[1].z > w[0].z), "layer tops do not climb");
    for (i, l) in paths.layers.iter().enumerate() {
        assert!(l.paths.iter().any(|p| p.feature == Some(Feature::OuterWall)), "layer {i} has no outer wall");
        assert!(l.paths.iter().all(|p| p.points.len() >= 2 && p.width_mm > 0.1 && p.width_mm < 2.0), "layer {i} has a degenerate path");
        assert!(l.time_s > 0.0, "layer {i} takes no time");
    }
    // The cube stands in the middle of the default 256 mm bed: its walls span about 20 mm each way.
    let b = paths.bounds().unwrap_or_else(|| panic!("no bounds"));
    for axis in 0..2 {
        let span = b.max[axis] - b.min[axis];
        assert!(span > 19.0 && span < 40.0, "span {span} on axis {axis} (skirt or brim counted)");
        assert!(b.min[axis] > 80.0 && b.max[axis] < 176.0, "not in the middle of the bed: {b:?}");
    }
    assert!(b.min[2].abs() < 1e-4 && (b.max[2] - 20.0).abs() < 1e-3, "{b:?}");
    // The totals name every feature printed; outer walls are the longest wall feature of a plain cube.
    let totals = paths.by_feature();
    let outer = totals.iter().find(|t| t.feature == Some(Feature::OuterWall)).unwrap_or_else(|| panic!("no outer wall total"));
    assert!(outer.paths >= 100 && outer.length_mm > 100.0 * 70.0, "{outer:?}");
    assert!(totals.iter().all(|t| t.paths > 0));
    // Travels run between consecutive paths of a layer.
    let l = &paths.layers[50];
    assert_eq!(l.travels().count(), l.paths.len() - 1);
    // The default printer: a 256 mm bed, 250 mm tall.
    assert_eq!(sliced.bed, [0.0, 0.0, 256.0, 256.0]);
    assert_eq!(sliced.printable_height, 250.0);
    // The cube's corner was at the origin; the slicer put its middle at the bed's middle.
    assert_eq!(sliced.offset, [118.0, 118.0, 0.0]);
    let b = paths.bounds().unwrap_or_else(|| panic!("no bounds"));
    assert!((b.min[0] - sliced.offset[0]).abs() < 10.0 && (b.max[0] - sliced.offset[0] - 20.0).abs() < 10.0, "{b:?}");
}

#[test]
fn feature_ids_round_trip_and_cover_every_feature() {
    for &f in Feature::ALL {
        assert_eq!(Feature::parse(f.id()), Some(f));
        assert!(!f.label().is_empty());
    }
    assert_eq!(Feature::parse("nothing"), None);
    let mut ids: Vec<&str> = Feature::ALL.iter().map(|f| f.id()).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), Feature::ALL.len(), "two features share an id");
}

/// A closed box `x` by `y` by `z` with one corner at the origin.
fn slab(x: f32, y: f32, z: f32) -> Part {
    let mut part = cube(1.0);
    for p in &mut part.positions {
        *p = [p[0] * x, p[1] * y, p[2] * z];
    }
    part
}

#[test]
fn a_placement_moves_and_turns_the_part_on_the_bed() {
    let parts = [slab(20.0, 10.0, 5.0)];
    let config = r#"{"layer_height": 0.2, "initial_layer_print_height": 0.2, "gcode_flavor": "klipper"}"#;
    let slice = |placement: Placement| default_slicer().slice(&Job { parts: &parts, config, placement, now: None });
    let centred = slice(Placement::default()).unwrap_or_else(|e| panic!("{e}"));
    let moved = slice(Placement { x: 30.0, y: -20.0, rotation_deg: 0.0 }).unwrap_or_else(|e| panic!("{e}"));
    assert!((moved.offset[0] - centred.offset[0] - 30.0).abs() < 1e-3, "{:?} vs {:?}", moved.offset, centred.offset);
    assert!((moved.offset[1] - centred.offset[1] + 20.0).abs() < 1e-3);
    let (a, b) = (centred.toolpaths.bounds().unwrap_or_default(), moved.toolpaths.bounds().unwrap_or_default());
    assert!((b.min[0] - a.min[0] - 30.0).abs() < 0.5 && (b.min[1] - a.min[1] + 20.0).abs() < 0.5, "{a:?} {b:?}");
    let turned = slice(Placement { x: 0.0, y: 0.0, rotation_deg: 90.0 }).unwrap_or_else(|e| panic!("{e}"));
    let c = turned.toolpaths.bounds().unwrap_or_default();
    // The 20 by 10 footprint stands the other way, still about the bed's centre.
    assert!(((c.max[0] - c.min[0]) - (a.max[1] - a.min[1])).abs() < 1.0, "{a:?} {c:?}");
    assert!(((c.max[1] - c.min[1]) - (a.max[0] - a.min[0])).abs() < 1.0, "{a:?} {c:?}");
    assert!((f32::midpoint(c.min[0], c.max[0]) - f32::midpoint(a.min[0], a.max[0])).abs() < 1.0);
    assert!(slice(Placement { x: f32::NAN, y: 0.0, rotation_deg: 0.0 }).is_err());
}

#[test]
fn pieces_of_one_extrusion_join_into_one_path() {
    use crate::toolpaths::{Feature, Path};
    let piece = |feature: Feature, width: f32, pts: &[[i32; 2]]| {
        let points: Vec<[f32; 2]> = pts.iter().map(|p| [p[0] as f32, p[1] as f32]).collect();
        let path = Path { feature: Some(feature), tool: 1, width_mm: width, speed_mm_s: 50.0, points };
        (path, pts[0], pts[pts.len() - 1])
    };
    let solid = Feature::InternalSolid;
    let joined = crate::sx::join_touching([
        // A zigzag handed over a stroke at a time: one path with its turns as corners.
        piece(solid, 0.42, &[[0, 0], [10, 0]]),
        piece(solid, 0.42, &[[10, 0], [10, 1]]),
        piece(solid, 0.42, &[[10, 1], [0, 1]]),
        // A different width starts a new path, though it touches.
        piece(solid, 0.38, &[[0, 1], [0, 2]]),
        // A gap starts a new path, though nothing else changes.
        piece(solid, 0.38, &[[5, 5], [6, 5]]),
        // So does another feature.
        piece(Feature::OuterWall, 0.38, &[[6, 5], [7, 5]]),
    ]);
    let points: Vec<usize> = joined.iter().map(|p| p.points.len()).collect();
    assert_eq!(points, [4, 2, 2, 2]);
    assert_eq!(joined[0].points, [[0.0, 0.0], [10.0, 0.0], [10.0, 1.0], [0.0, 1.0]]);
}
