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
    let job = Job { parts: &parts, config: r#"{"layer_height": 0.2, "initial_layer_print_height": 0.2, "gcode_flavor": "klipper"}"# };
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
    for (parts, config) in
        [(vec![broken], "{}"), (vec![nan], "{}"), (vec![], "{}"), (vec![cube(10.0)], "not json"), (vec![cube(10.0)], r#"{"layer_height": "tall"}"#)]
    {
        let job = Job { parts: &parts, config };
        assert!(default_slicer().slice(&job).is_err(), "{config} with {} parts", parts.len());
    }
}

#[test]
fn a_flat_part_has_nothing_to_print() {
    let flat = Part { name: "flat".into(), positions: vec![[0., 0., 0.], [10., 0., 0.], [0., 10., 0.]], triangles: vec![[0, 1, 2]] };
    let parts = [flat];
    let job = Job { parts: &parts, config: "{}" };
    assert!(default_slicer().slice(&job).is_err());
}

#[test]
fn the_engine_version_named_is_the_tag_the_workspace_pins() {
    let manifest = include_str!("../../../Cargo.toml");
    let pinned = manifest.lines().find(|l| l.starts_with("sx-core = ")).unwrap_or_default();
    assert!(pinned.contains(&format!("tag = \"engine-v{}\"", sx::ENGINE_VERSION)), "{pinned}");
}
