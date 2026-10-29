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

/// How many placements the scene makes, over every shared piece.
fn placements(scene: &GpuScene) -> usize {
    scene.instanced.iter().map(|m| m.instances.len() / crate::gpu::INSTANCE_SIZE).sum()
}

/// Whether a shared piece is a straight run: one unit long along +x, as wide to each side.
fn is_run(m: &crate::gpu::InstancedMesh) -> bool {
    let [lo, hi] = m.tris.bounds;
    lo[0].abs() < 1e-6 && (hi[0] - 1.0).abs() < 1e-6 && (lo[1] + hi[1]).abs() < 1e-6
}

/// How many placements the scene makes of the pieces `which` picks.
fn placements_of(scene: &GpuScene, which: impl Fn(&crate::gpu::InstancedMesh) -> bool) -> usize {
    scene.instanced.iter().filter(|m| which(m)).map(|m| m.instances.len() / crate::gpu::INSTANCE_SIZE).sum()
}

/// The scene of the first `shown` layers, built as the canvas builds it.
fn toolpath_scene(h: &HeldSlice, travels: bool, shown: usize) -> Result<GpuScene, String> {
    Ok(Layered::build(h, travels, Budget::default())?.shown(shown))
}

fn scene_of(h: &HeldSlice, travels: bool) -> GpuScene {
    toolpath_scene(h, travels, usize::MAX).unwrap_or_else(|e| panic!("{e}"))
}

fn one_layer(points: Vec<[f32; 2]>) -> HeldSlice {
    held_with(vec![Layer { z: 0.2, height: 0.2, time_s: 1.0, paths: vec![path(Feature::OuterWall, points)] }])
}

#[test]
fn straight_open_beads_are_placed_runs_and_caps_and_travels_join_the_paths() {
    let h = held(2, 3, 4);
    let scene = scene_of(&h, true);
    let (segments, ends) = (2 * 3 * 3, 2 * 3 * 2);
    // Every piece is a placement of a shared one (one width, one layer height): a run per
    // segment and a cap per end; collinear points make no joins, and no bead is a triangle.
    assert_eq!(placements_of(&scene, is_run), segments);
    assert_eq!(placements(&scene), segments + ends);
    assert_eq!(scene.instanced.len(), 3, "the run, the first cap and the last cap");
    assert!(scene.tris.is_empty());
    // Two travels per layer (between three paths), plus the bed grid and outline under everything.
    assert_eq!(scene.lines.len(), 2 * 2 * LINE_BYTES);
    assert!(scene.under.len() >= 4 * LINE_BYTES);
    let without = scene_of(&h, false);
    assert_eq!(without.lines.len(), 0);
    assert_eq!(placements(&without), placements(&scene));
}

/// Where a placement puts the piece-frame point `p`, over every placement of the pieces `which`
/// picks.
fn placed(scene: &GpuScene, which: impl Fn(&crate::gpu::InstancedMesh) -> bool, p: [f32; 3]) -> Vec<[f32; 3]> {
    let mut out = Vec::new();
    for mesh in scene.instanced.iter().filter(|m| which(m)) {
        for one in mesh.instances.as_chunks::<{ crate::gpu::INSTANCE_SIZE }>().0 {
            let f = |i: usize| f32::from_le_bytes([one[4 * i], one[4 * i + 1], one[4 * i + 2], one[4 * i + 3]]);
            let rows: [[f32; 4]; 3] = std::array::from_fn(|r| std::array::from_fn(|c| f(4 * r + c)));
            out.push(rows.map(|r| r[0] * p[0] + r[1] * p[1] + r[2] * p[2] + r[3]));
        }
    }
    out
}

#[test]
fn a_placement_lays_the_unit_run_along_its_segment() {
    let mut h = one_layer(vec![[2.0, 3.0], [2.0, 8.0]]);
    h.sliced.toolpaths.layers[0].z = 0.4;
    let scene = scene_of(&h, false);
    let close = |a: [f32; 3], b: [f32; 3]| a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-5);
    // The run's start at the segment's start, its end at the segment's end, its top at the layer's,
    // its sideways axis the path's left (going +y, that is -x).
    assert!(close(placed(&scene, is_run, [0.0, 0.0, 0.0])[0], [2.0, 3.0, 0.4]));
    assert!(close(placed(&scene, is_run, [1.0, 0.0, 0.0])[0], [2.0, 8.0, 0.4]));
    assert!(close(placed(&scene, is_run, [0.0, 1.0, 0.0])[0], [1.0, 3.0, 0.4]));
    // Every face of the shared run runs along it, so stretching it leaves its normals alone.
    let run = scene.instanced.iter().find(|m| is_run(m)).map(|m| m.tris.clone()).unwrap_or_default();
    for v in run.vertices.as_chunks::<{ crate::gpu::SHARED_SIZE }>().0 {
        let nx = f32::from_le_bytes([v[12], v[13], v[14], v[15]]);
        assert!(nx.abs() < 1e-6, "a face of the shared run faces along it ({nx})");
    }
}

#[test]
fn a_corner_gets_a_fan_on_its_outside_and_a_closed_loop_gets_one_at_its_seam() {
    // An open right angle: two runs, one join, two caps.
    let scene = scene_of(&one_layer(vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0]]), false);
    assert_eq!(join_steps(std::f32::consts::FRAC_PI_2), 2);
    assert_eq!(placements_of(&scene, is_run), 2);
    assert_eq!(placements(&scene), 5);
    // A closed square: four runs, four joins of one shared fan, no caps.
    let scene = scene_of(&one_layer(vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0], [0.0, 0.0]]), false);
    assert_eq!(placements(&scene), 8);
    assert_eq!(scene.instanced.len(), 2, "one run and one left-turn fan");
    // A hairpin turns 180 degrees: its fan takes several steps so it stays round.
    assert_eq!(join_steps(std::f32::consts::PI), 4);
    let scene = scene_of(&one_layer(vec![[0.0, 0.0], [10.0, 0.0], [0.5, 0.0]]), false);
    assert_eq!(placements(&scene), 5);
}

#[test]
fn a_join_sits_at_its_corner_and_fills_the_outside_of_the_turn() {
    // Going +x, then turning left to +y at (10, 0): the gap is on the right, below the corner.
    let scene = scene_of(&one_layer(vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0]]), false);
    // Neither a run nor a cap (which is flat, at x = 0): reaching ahead and to the right.
    let is_fan = |m: &crate::gpu::InstancedMesh| !is_run(m) && m.tris.bounds[1][0] > 0.1 && m.tris.bounds[0][1] < -0.1;
    let fan = scene.instanced.iter().find(|m| is_fan(m)).map(|m| m.tris.clone()).unwrap_or_else(|| panic!("no fan to the right of its path"));
    // The fan's own frame has the incoming step along +x: its outside is to the right (-y) and
    // ahead (+x), within the bead's half width of the corner.
    let [lo, hi] = fan.bounds;
    assert!(lo[1] < -0.2 && hi[0] > 0.2 && lo[0] > -1e-4 && hi[1] < 1e-4, "{:?}", fan.bounds);
    let corner = placed(&scene, is_fan, [0.0, 0.0, 0.0]);
    assert!(corner.iter().any(|p| (p[0] - 10.0).abs() < 1e-5 && p[1].abs() < 1e-5 && (p[2] - 0.2).abs() < 1e-5), "{corner:?}");
}

#[test]
fn every_corner_that_turns_gets_a_join_however_gentle() {
    // A chord of a fine curve turns a degree or so: left open, its outside gap shows as a dark seam
    // across the bead up close. Only a step straight on (under STRAIGHT_ON) goes without.
    let turned = |degrees: f32| {
        let (s, c) = degrees.to_radians().sin_cos();
        one_layer(vec![[0.0, 0.0], [10.0, 0.0], [10.0 + 10.0 * c, 10.0 * s]])
    };
    assert_eq!(placements(&scene_of(&turned(1.0), false)), 5);
    assert_eq!(placements(&scene_of(&turned(0.001), false)), 4);
    assert!(join_wanted(1f32.to_radians()) && !join_wanted(0.001f32.to_radians()));
    // A quarter-degree turn takes the smallest fan, FAN_STEP_DEGREES: rounded up, it covers the
    // whole outside of the turn, and what it sweeps past it lies inside the next run's bead.
    let scene = scene_of(&turned(0.25), false);
    let fan = scene.instanced.iter().find(|m| !is_run(m) && m.tris.bounds[1][0] > 0.0 && m.tris.bounds[0][1] < -0.1).map(|m| m.tris.bounds);
    let reach = fan.unwrap_or_else(|| panic!("no fan"))[1][0];
    let step = f32::from(FAN_STEP_DEGREES).to_radians();
    assert!((reach - 0.21 * step.sin()).abs() < 1e-4, "the fan reaches {reach} mm ahead");
    assert_eq!(fan_degrees(0.25f32.to_radians()), FAN_STEP_DEGREES);
    assert_eq!(fan_degrees(3.0f32.to_radians()), 4);
    assert_eq!(fan_degrees(180.0f32.to_radians()), 180);
}

#[test]
fn every_piece_triangle_faces_the_way_its_normal_points() {
    let square = held_with(vec![Layer {
        z: 0.2,
        height: 0.2,
        time_s: 1.0,
        paths: vec![
            path(Feature::OuterWall, vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0], [0.0, 0.0]]),
            path(Feature::SparseInfill, vec![[1.0, 1.0], [9.0, 9.0], [9.0, 1.0]]),
        ],
    }]);
    let scene = scene_of(&square, false);
    assert!(scene.instanced.len() >= 5, "runs, fans of both sides, caps of both ends");
    let f = |b: &[u8]| f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    for mesh in &scene.instanced {
        let shared = &mesh.tris;
        let at = |i: u32| {
            let v = &shared.vertices[i as usize * crate::gpu::SHARED_SIZE..];
            ([f(&v[0..]), f(&v[4..]), f(&v[8..])], [f(&v[12..]), f(&v[16..]), f(&v[20..])])
        };
        for (t, w) in shared.indices.as_chunks::<3>().0.iter().enumerate() {
            let ((a, n), (b, _), (c, _)) = (at(w[0]), at(w[1]), at(w[2]));
            let (u, v) = ([b[0] - a[0], b[1] - a[1], b[2] - a[2]], [c[0] - a[0], c[1] - a[1], c[2] - a[2]]);
            let cross = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
            assert!(cross[0] * n[0] + cross[1] * n[1] + cross[2] * n[2] > 0.0, "triangle {t} at {a:?} winds against its normal {n:?}");
            assert!((n[0] * n[0] + n[1] * n[1] + n[2] * n[2] - 1.0).abs() < 1e-3, "normal {n:?} is not unit");
        }
    }
}

#[test]
fn the_preview_budget_follows_the_device_and_a_print_past_it_is_refused_not_thinned() {
    // The clamp pair at 50% gyroid (1,969,754 segments, 1,961,844 joins, 14,866 ends) was past
    // the old fixed 128 MiB and fell to lines; the desktop budget for 256 MiB buffers holds it.
    let pair = Cost { segments: 1_969_754, joins: 1_961_844, caps: 14_866 };
    let desktop = Budget::of(256 << 20, false);
    assert_eq!((desktop.total, desktop.buffer), (2 << 30, 256 << 20));
    assert!(pair.bytes() > 128 << 20 && pair.bytes() <= desktop.total);
    // The web keeps 128 MiB, and so does a device not yet known.
    assert_eq!(Budget::default(), Budget::of(u64::MAX, true));
    assert_eq!(Budget::default().total, 128 << 20);
    // Past the budget the preview says so, with both sizes, rather than drawing less.
    let tiny = Budget { total: 1024, buffer: 1024 };
    let e = Layered::build(&held(2, 3, 4), false, tiny).err().unwrap_or_default();
    assert!(e.contains("too large to preview") && e.contains("MiB"), "{e}");
    // One piece's placements past one buffer is refused too, though the total fits.
    let narrow = Budget { total: 2 << 30, buffer: crate::gpu::INSTANCE_SIZE };
    assert!(Layered::build(&held(2, 3, 4), false, narrow).is_err());
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


#[test]
fn the_layer_slider_leaves_the_layers_above_out_whole() {
    // Three layers, one path of two segments each: showing two draws the first two layers' pieces
    // and nothing of the third, so no cut through it can leave a sliver of it behind.
    let h = held(3, 1, 3);
    let tops = |shown: usize| {
        let scene = toolpath_scene(&h, true, shown).unwrap_or_else(|e| panic!("{e}"));
        let mut tops: Vec<f32> = Vec::new();
        for mesh in &scene.instanced {
            for one in mesh.instances.as_chunks::<{ crate::gpu::INSTANCE_SIZE }>().0 {
                tops.push(f32::from_le_bytes([one[44], one[45], one[46], one[47]]));
            }
        }
        (placements(&scene), tops, scene.lines.len())
    };
    let z: Vec<f32> = h.sliced.toolpaths.layers.iter().map(|l| l.z).collect();
    let (all, _, _) = tops(usize::MAX);
    let (two, top_two, travel_two) = tops(2);
    assert_eq!(two * 3, all * 2);
    assert!(top_two.iter().all(|t| (t - z[0]).abs() < 1e-6 || (t - z[1]).abs() < 1e-6), "{top_two:?}");
    // One path per layer: no travel moves within a layer, in either case.
    assert_eq!(travel_two, 0);
}

#[test]
fn beads_of_nearly_one_width_share_their_pieces() {
    // Widths the slicer varies continuously (gap fill) round to WIDTH_STEP: a hundred widths
    // between 0.400 and 0.4045 mm make one run, one pair of caps and one fan, not a hundred of each.
    let paths: Vec<Path> = (0..100)
        .map(|i| {
            let mut p = path(Feature::GapFill, vec![[0.0, i as f32], [5.0, i as f32], [10.0, i as f32 + 0.5]]);
            p.width_mm = 0.4 + 0.000045 * i as f32;
            p
        })
        .collect();
    let scene = scene_of(&held_with(vec![Layer { z: 0.2, height: 0.2, time_s: 1.0, paths }]), false);
    assert_eq!(placements(&scene), 100 * 5);
    assert!(scene.instanced.len() <= 4, "{} pieces", scene.instanced.len());
    assert!((bead_half(0.42f32.next_down()) - bead_half(0.42)).abs() < 1e-9);
}

/// The Print Settings window applies changes as they are made; Cancel puts the design's overrides
/// and the settings files back as they were when it opened, and OK keeps them.
#[test]
fn print_settings_cancel_restores_and_ok_keeps() {
    use serde_json::json;
    let mut s = solidcraft_engine::Session::default();
    s.execute("print.settings.set", &json!({"set": {"layer_height": 0.2}})).unwrap_or_else(|e| panic!("{e}"));
    let mut app = SolveApp::new(s, crate::Services::default());
    app.print_workspace.settings.config_files = vec!["/profiles/printer.json".into()];
    let overlay = |app: &SolveApp| app.session.doc.print.overlay.clone();
    let before = overlay(&app);

    crate::print_settings_window::open(&mut app);
    app.run("print.settings.set", json!({"set": {"layer_height": 0.12, "wall_loops": 4}})).unwrap_or_else(|e| panic!("{e}"));
    app.print_workspace.settings.config_files.push("/profiles/filament.json".into());
    crate::print_settings_window::close(&mut app, false);
    assert_eq!(overlay(&app), before, "Cancel puts the overrides back");
    assert_eq!(app.print_workspace.settings.config_files, vec!["/profiles/printer.json".to_string()], "and the settings files");

    crate::print_settings_window::open(&mut app);
    app.run("print.settings.clear", json!({})).unwrap_or_else(|e| panic!("{e}"));
    crate::print_settings_window::close(&mut app, false);
    assert_eq!(overlay(&app), before, "Cancel brings back overrides that were cleared");

    crate::print_settings_window::open(&mut app);
    app.run("print.settings.set", json!({"set": {"wall_loops": 3}})).unwrap_or_else(|e| panic!("{e}"));
    crate::print_settings_window::close(&mut app, true);
    assert_eq!(overlay(&app).get("wall_loops"), Some(&json!(3)), "OK keeps the change");
    assert!(!app.print_settings_window.open);
}

/// Print from the ribbon (or a shortcut) asks before anything goes to the printer: it opens the
/// "start printing?" question and starts no upload, with a slice ready and a printer that would
/// take it.
#[test]
fn print_from_the_ribbon_asks_first() {
    use serde_json::json;
    use solidcraft_engine::printer_host::{CameraFrame, JobState, PrinterHost, PrinterInfo, PrinterStatus, Webcam};
    use std::sync::Mutex;
    /// A printer that counts uploads and answers nothing else.
    #[derive(Default)]
    struct Counting(Mutex<u32>);
    impl PrinterHost for Counting {
        fn info(&self) -> PrinterInfo {
            PrinterInfo { name: "test".into(), url: "http://test".into() }
        }
        fn webcams(&self) -> Vec<Webcam> {
            Vec::new()
        }
        fn script(&self, _gcode: &str) -> Result<u64, String> {
            Err("not in this test".into())
        }
        fn watch_camera(&self, _url: &str, _on: bool) {}
        fn camera_frame(&self) -> Result<Option<CameraFrame>, String> {
            Ok(None)
        }
        fn upload(&self, _name: &str, _gcode: Vec<u8>, _start: bool) -> Result<u64, String> {
            *self.0.lock().unwrap_or_else(|e| e.into_inner()) += 1;
            Ok(1)
        }
        fn cancel_print(&self) -> Result<u64, String> {
            Err("not in this test".into())
        }
        fn job(&self, _id: u64) -> Option<JobState> {
            None
        }
        fn status(&self) -> PrinterStatus {
            PrinterStatus { reachable: true, state: "standby".into(), ..Default::default() }
        }
    }
    let host = Arc::new(Counting::default());
    let mut s = solidcraft_engine::Session::default();
    s.set_printer_host(Some(host.clone()));
    s.execute("solid.box", &json!({"length": 10, "width": 10, "height": 10})).unwrap_or_else(|e| panic!("{e}"));
    s.execute("print.slice", &json!({"settings": {"layer_height": 0.2, "initial_layer_print_height": 0.2, "gcode_flavor": "klipper"}}))
        .unwrap_or_else(|e| panic!("{e}"));
    let mut app = SolveApp::new(s, crate::Services::default());
    app.start("print.start");
    assert!(app.print_workspace.confirm_print, "the question is open");
    assert_eq!(*host.0.lock().unwrap_or_else(|e| e.into_inner()), 0, "nothing was sent before the answer");
}
