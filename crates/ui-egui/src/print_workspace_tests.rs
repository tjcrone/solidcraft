use super::*;
use solidcraft_engine::manufacture::{Path, Sliced};

fn held(layers: usize, paths_per_layer: usize, points_per_path: usize) -> HeldSlice {
    let layers = (0..layers)
        .map(|i| Layer {
            z: 0.2 * (i + 1) as f32,
            height: 0.2,
            time_s: 1.0,
            paths: (0..paths_per_layer)
                .map(|k| Path {
                    feature: Some(if k == 0 { Feature::OuterWall } else { Feature::SparseInfill }),
                    tool: 1,
                    width_mm: 0.42,
                    speed_mm_s: 60.0,
                    points: (0..points_per_path).map(|j| [10.0 + j as f32, 10.0 + k as f32]).collect(),
                })
                .collect(),
        })
        .collect();
    HeldSlice {
        sliced: Sliced { toolpaths: Toolpaths { layers }, bed: [0.0, 0.0, 100.0, 100.0], printable_height: 100.0, ..Default::default() },
        doc: Arc::new(solidcraft_engine::doc::Document::new("t")),
        bodies: Vec::new(),
        config: "{}".into(),
        slicer: "test".into(),
    }
}

#[test]
fn seconds_read_as_hours_minutes_seconds() {
    assert_eq!(hms(0.0), "0:00:00");
    assert_eq!(hms(59.4), "0:00:59");
    assert_eq!(hms(3661.0), "1:01:01");
    assert_eq!(hms(-5.0), "0:00:00");
    assert_eq!(hms(f64::NAN), "0:00:00");
}

#[test]
fn every_feature_has_its_own_colour() {
    let mut colours: Vec<[u8; 4]> = Feature::ALL.iter().map(|f| feature_color(Some(*f))).collect();
    colours.push(feature_color(None));
    colours.sort_unstable();
    let n = colours.len();
    colours.dedup();
    // Custom and an unknown feature share one colour on purpose.
    assert_eq!(colours.len(), n - 1);
}

#[test]
fn beads_have_a_top_and_two_sides_and_travels_join_the_paths() {
    let h = held(2, 3, 4);
    let scene = toolpath_scene(&h, true).unwrap_or_else(|e| panic!("{e}"));
    let segments = 2 * 3 * 3;
    assert_eq!(scene.tris.len(), segments * 6 * TRI_BYTES);
    // Two travels per layer (between three paths), plus the bed grid and outline under everything.
    assert_eq!(scene.lines.len(), 2 * 2 * LINE_BYTES);
    assert!(scene.under.len() >= 4 * LINE_BYTES);
    let without = toolpath_scene(&h, false).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(without.lines.len(), 0);
    assert_eq!(without.tris.len(), scene.tris.len());
}

#[test]
fn a_print_past_the_bead_budget_still_shows_as_ribbons_or_lines() {
    // Enough segments that beads would pass the budget while ribbons fit.
    let per_layer = MAX_GEOMETRY_BYTES / (6 * TRI_BYTES) / 100 + 1;
    let h = held(100, 1, per_layer + 1);
    let scene = toolpath_scene(&h, false).unwrap_or_else(|e| panic!("{e}"));
    let segments = 100 * per_layer;
    assert_eq!(scene.tris.len(), segments * 2 * TRI_BYTES, "ribbons");
    let per_layer = MAX_GEOMETRY_BYTES / (2 * TRI_BYTES) / 100 + 1;
    let h = held(100, 1, per_layer + 1);
    let scene = toolpath_scene(&h, false).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(scene.tris.len(), 0, "lines");
    assert_eq!(scene.lines.len(), 100 * per_layer * LINE_BYTES);
}

#[test]
fn a_degenerate_bed_draws_no_grid() {
    let mut gpu = GpuScene::default();
    bed(&mut gpu, [0.0, 0.0, 0.0, 0.0]);
    bed(&mut gpu, [f32::NAN, 0.0, 10.0, 10.0]);
    assert!(gpu.under.is_empty());
    bed(&mut gpu, [0.0, 0.0, 20.0, 10.0]);
    // Grid lines at 0, 10, 20 in X and 0, 10 in Y, plus the four edges.
    assert_eq!(gpu.under.len(), (3 + 2 + 4) * LINE_BYTES);
}
