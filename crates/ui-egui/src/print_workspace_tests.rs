use super::*;
use solidcraft_engine::print::{Path, Sliced};

fn held_with(layers: Vec<Layer>) -> HeldSlice {
    HeldSlice {
        sliced: Sliced {
            toolpaths: Toolpaths { layers, file_beads: Vec::new() },
            bed: [0.0, 0.0, 100.0, 100.0],
            printable_height: 100.0,
            ..Default::default()
        },
        doc: Arc::new(solidcraft_engine::doc::Document::new("t")),
        bodies: Vec::new(),
        config: "{}".into(),
        slicer: "test".into(),
    }
}

fn path(feature: Feature, points: Vec<[f32; 2]>) -> Path {
    Path { feature: Some(feature), tool: 1, width_mm: 0.42, speed_mm_s: 60.0, points }
}

/// Straight open paths: `paths_per_layer` of them, each `points_per_path` collinear points.
fn held(layers: usize, paths_per_layer: usize, points_per_path: usize) -> HeldSlice {
    held_with(
        (0..layers)
            .map(|i| Layer {
                z: 0.2 * (i + 1) as f32,
                height: 0.2,
                time_s: 1.0,
                paths: (0..paths_per_layer)
                    .map(|k| {
                        path(
                            if k == 0 { Feature::OuterWall } else { Feature::SparseInfill },
                            (0..points_per_path).map(|j| [10.0 + j as f32, 10.0 + k as f32]).collect(),
                        )
                    })
                    .collect(),
            })
            .collect(),
    )
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
fn straight_open_beads_have_a_profile_and_two_caps_and_travels_join_the_paths() {
    let h = held(2, 3, 4);
    let scene = toolpath_scene(&h, true).unwrap_or_else(|e| panic!("{e}"));
    let (segments, ends) = (2 * 3 * 3, 2 * 3 * 2);
    // Collinear points make no joins.
    assert_eq!(scene.tris.len(), (segments * BEAD_VERTS + ends * CAP_VERTS) * crate::gpu::TRI_SIZE);
    // Two travels per layer (between three paths), plus the bed grid and outline under everything.
    assert_eq!(scene.lines.len(), 2 * 2 * LINE_BYTES);
    assert!(scene.under.len() >= 4 * LINE_BYTES);
    let without = toolpath_scene(&h, false).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(without.lines.len(), 0);
    assert_eq!(without.tris.len(), scene.tris.len());
}

#[test]
fn a_corner_gets_a_fan_on_its_outside_and_a_closed_loop_gets_one_at_its_seam() {
    // An open right angle: one join of one step, two caps.
    let corner = held_with(vec![Layer {
        z: 0.2,
        height: 0.2,
        time_s: 1.0,
        paths: vec![path(Feature::OuterWall, vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0]])],
    }]);
    let scene = toolpath_scene(&corner, false).unwrap_or_else(|e| panic!("{e}"));
    let right = join_steps(std::f32::consts::FRAC_PI_2);
    assert_eq!(right, 2);
    assert_eq!(scene.tris.len(), (2 * BEAD_VERTS + right * JOIN_VERTS + 2 * CAP_VERTS) * crate::gpu::TRI_SIZE);
    // A closed square: four segments, four joins, no caps.
    let square = held_with(vec![Layer {
        z: 0.2,
        height: 0.2,
        time_s: 1.0,
        paths: vec![path(Feature::OuterWall, vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0], [0.0, 0.0]])],
    }]);
    let scene = toolpath_scene(&square, false).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(scene.tris.len(), (4 * BEAD_VERTS + 4 * right * JOIN_VERTS) * crate::gpu::TRI_SIZE);
    // A hairpin turns 180 degrees: the fan takes several steps so it stays round.
    let hairpin =
        held_with(vec![Layer { z: 0.2, height: 0.2, time_s: 1.0, paths: vec![path(Feature::OuterWall, vec![[0.0, 0.0], [10.0, 0.0], [0.5, 0.0]])] }]);
    let scene = toolpath_scene(&hairpin, false).unwrap_or_else(|e| panic!("{e}"));
    let steps = join_steps(std::f32::consts::PI);
    assert_eq!(steps, 4);
    assert_eq!(scene.tris.len(), (2 * BEAD_VERTS + steps * JOIN_VERTS + 2 * CAP_VERTS) * crate::gpu::TRI_SIZE);
}

#[test]
fn every_bead_triangle_faces_the_way_its_normal_points() {
    let square = held_with(vec![Layer {
        z: 0.2,
        height: 0.2,
        time_s: 1.0,
        paths: vec![
            path(Feature::OuterWall, vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0], [0.0, 0.0]]),
            path(Feature::SparseInfill, vec![[1.0, 1.0], [9.0, 9.0]]),
        ],
    }]);
    let scene = toolpath_scene(&square, false).unwrap_or_else(|e| panic!("{e}"));
    let f = |b: &[u8]| f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    let vertex = |i: usize| {
        let v = &scene.tris[i * crate::gpu::TRI_SIZE..];
        ([f(&v[0..]), f(&v[4..]), f(&v[8..])], [f(&v[12..]), f(&v[16..]), f(&v[20..])])
    };
    let count = scene.tris.len() / crate::gpu::TRI_SIZE;
    assert_eq!(count % 3, 0);
    for t in 0..count / 3 {
        let ((a, n), (b, _), (c, _)) = (vertex(3 * t), vertex(3 * t + 1), vertex(3 * t + 2));
        let (u, v) = ([b[0] - a[0], b[1] - a[1], b[2] - a[2]], [c[0] - a[0], c[1] - a[1], c[2] - a[2]]);
        let cross = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
        let dot = cross[0] * n[0] + cross[1] * n[1] + cross[2] * n[2];
        assert!(dot > 0.0, "triangle {t} at {a:?} winds against its normal {n:?}");
        assert!((n[0] * n[0] + n[1] * n[1] + n[2] * n[2] - 1.0).abs() < 1e-3, "normal {n:?} is not unit");
    }
}

#[test]
fn a_print_past_the_bead_budget_still_shows_with_less_profile_or_as_lines() {
    // One straight open path per layer, long enough that the finer details pass the budget.
    for (finer, per_segment, per_cap) in [(Detail::Beads, HALF_VERTS, HALF_CAP_VERTS), (Detail::Half, FLAT_VERTS, 0)] {
        // Just past what the finer detail allows.
        let per_layer = MAX_GEOMETRY_BYTES / bytes_per_segment(finer) / 100 + 1;
        let h = held(100, 1, per_layer + 1);
        let scene = toolpath_scene(&h, false).unwrap_or_else(|e| panic!("{e}"));
        let segments = 100 * per_layer;
        assert_eq!(scene.tris.len(), (segments * per_segment + 100 * 2 * per_cap) * crate::gpu::TRI_SIZE, "{segments} segments");
    }
    let per_layer = MAX_GEOMETRY_BYTES / bytes_per_segment(Detail::Flat) / 100 + 1;
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

#[test]
fn the_view_moves_by_the_change_in_placement() {
    let d = view_shift([0.0; 3], [118.0, 118.0, 0.0]);
    assert_eq!((d.x, d.y, d.z), (118.0, 118.0, 0.0));
    let back = view_shift([118.0, 118.0, 0.0], [0.0; 3]);
    assert_eq!((back.x, back.y, back.z), (-118.0, -118.0, 0.0));
    let between = view_shift([118.0, 118.0, 0.0], [120.0, 110.0, 1.0]);
    assert_eq!((between.x, between.y, between.z), (2.0, -8.0, 1.0));
}
