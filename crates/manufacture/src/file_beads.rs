//! The beads the finished G-code commands outside its sliced layers: a purge line from the start
//! template, a wipe from the end template. The slicer never planned them, so the toolpaths do not
//! have them; this reads them back from the file the way the printer will move. Only beads that
//! land inside the bed's printable area count: a purge into a chute or a wipe on a pad outside it
//! is not plastic on the print bed.
//!
//! The regions read are the start block (before the first `;LAYER_CHANGE`) and the end block
//! (after the engine's `; end` line). What the printer's own macros extrude is not in the file.

use crate::toolpaths::FileBead;

/// Reads `gcode` for extruding moves inside `bed` (`[min_x, min_y, max_x, max_y]` mm) outside the
/// sliced layers. `filament_diameter` (mm) turns the extruded length into a bead width.
pub fn read(gcode: &[u8], bed: [f32; 4], filament_diameter: f32) -> Vec<FileBead> {
    const TOLERANCE: f64 = 0.01;
    let area = f64::from(filament_diameter.clamp(0.5, 5.0)).powi(2) * std::f64::consts::FRAC_PI_4;
    let inside = |x: f64, y: f64| {
        x >= f64::from(bed[0]) - TOLERANCE
            && x <= f64::from(bed[2]) + TOLERANCE
            && y >= f64::from(bed[1]) - TOLERANCE
            && y <= f64::from(bed[3]) + TOLERANCE
    };
    let mut beads: Vec<FileBead> = Vec::new();
    let mut relative = false;
    let mut e_relative = false;
    let mut pos: [Option<f64>; 3] = [None; 3];
    let mut e: f64 = 0.0;
    let mut in_layers = false;
    for raw in gcode.split(|&b| b == b'\n') {
        let line = String::from_utf8_lossy(raw);
        let trimmed = line.trim();
        if trimmed == ";LAYER_CHANGE" {
            in_layers = true;
            continue;
        }
        if trimmed == "; end" {
            in_layers = false;
            continue;
        }
        let code = line.split(';').next().unwrap_or("").trim();
        let mut words = code.split_ascii_whitespace();
        let Some(cmd) = words.next() else { continue };
        match cmd {
            "G90" => relative = false,
            "G91" => relative = true,
            "M82" => e_relative = false,
            "M83" => e_relative = true,
            "G28" => pos = [None; 3],
            "G92" => {
                for w in words {
                    match w.as_bytes().first() {
                        Some(b'E') => e = number(w).unwrap_or(e),
                        _ => {
                            if let Some(i) = axis(w) {
                                pos[i] = number(w);
                            }
                        }
                    }
                }
            }
            "G0" | "G1" | "G2" | "G3" => {
                let from = pos;
                let mut extruded = 0.0;
                for w in words {
                    if w.as_bytes().first() == Some(&b'E') {
                        let Some(v) = number(w) else { continue };
                        if e_relative {
                            extruded = v;
                        } else {
                            extruded = v - e;
                            e = v;
                        }
                        continue;
                    }
                    let Some(i) = axis(w) else { continue };
                    let Some(v) = number(w) else { continue };
                    pos[i] = if relative { pos[i].map(|p| p + v) } else { Some(v) };
                }
                if in_layers || extruded <= 0.0 {
                    continue;
                }
                let ([Some(x0), Some(y0), Some(z0)], [Some(x1), Some(y1), Some(z1)]) = (from, pos) else { continue };
                let len = ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt();
                if len < 1e-3 || !inside(x0, y0) || !inside(x1, y1) {
                    continue;
                }
                // The bead's height is the nozzle's height over the bed, within reason.
                let height = z1.clamp(0.05, 1.0);
                let width = (extruded * area / (len * height)).clamp(0.1, 3.0) as f32;
                let a = [x0 as f32, y0 as f32, z0 as f32];
                let b = [x1 as f32, y1 as f32, z1 as f32];
                match beads.last_mut() {
                    Some(last) if last.points.last() == Some(&a) && (last.width_mm - width).abs() < 0.05 => last.points.push(b),
                    _ => beads.push(FileBead { points: vec![a, b], width_mm: width }),
                }
            }
            _ => {}
        }
    }
    beads
}

fn axis(word: &str) -> Option<usize> {
    match word.as_bytes().first()? {
        b'X' => Some(0),
        b'Y' => Some(1),
        b'Z' => Some(2),
        _ => None,
    }
}

fn number(word: &str) -> Option<f64> {
    word.get(1..)?.parse::<f64>().ok().filter(|v| v.is_finite())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BED: [f32; 4] = [0.0, 0.0, 200.0, 200.0];

    #[test]
    fn a_purge_line_in_the_start_block_is_one_bead_and_layers_are_left_alone() {
        let gcode = b"G28\nM83\nG90\nG0 X10 Y10 Z0.2 F6000\nG1 E3 F1800\nG1 X60 E2 F3000\nG1 Y12 E0.08\nG1 X10 E2\nG1 Z1\n;LAYER_CHANGE\nG1 X20 Y20 E5\nG1 X30 E5\n; end\nG1 E-3\nG0 X50 Y50 Z10\nG1 X80 E4\n";
        let beads = read(gcode, BED, 1.75);
        assert_eq!(beads.len(), 2, "{beads:?}");
        assert_eq!(beads[0].points.len(), 4, "{:?}", beads[0]);
        assert_eq!(beads[0].points[0], [10.0, 10.0, 0.2]);
        assert_eq!(beads[0].points[3], [10.0, 12.0, 0.2]);
        // 2 mm of 1.75 mm filament over 50 mm at 0.2 mm high: 4.81 mm3 / 10 mm2 = 0.48 mm wide.
        assert!((beads[0].width_mm - 0.48).abs() < 0.02, "{}", beads[0].width_mm);
        // The end block's move at Z 10 counts, with a height capped at 1 mm.
        assert_eq!(beads[1].points, vec![[50.0, 50.0, 10.0], [80.0, 50.0, 10.0]]);
    }

    #[test]
    fn moves_outside_the_bed_retractions_and_travels_are_not_beads() {
        let gcode = b"M83\nG90\nG0 X10 Y250 Z0.3\nG1 X100 E5\nG0 X10 Y10\nG1 X100 E-2\nG1 X150\nG1 X160 E0.5\n";
        let beads = read(gcode, BED, 1.75);
        assert_eq!(beads.len(), 1, "{beads:?}");
        assert_eq!(beads[0].points, vec![[150.0, 10.0, 0.3], [160.0, 10.0, 0.3]]);
    }

    #[test]
    fn absolute_extrusion_and_relative_moves_are_followed() {
        let gcode = b"M82\nG90\nG0 X10 Y10 Z0.2\nG92 E0\nG1 X20 E1\nG1 X30 E1\nG91\nG1 X10 E2\nG1 Y5 E2\n";
        let beads = read(gcode, BED, 1.75);
        // The second absolute E does not grow, so only the first move and the relative one extrude.
        assert_eq!(beads.len(), 2, "{beads:?}");
        assert_eq!(beads[0].points, vec![[10.0, 10.0, 0.2], [20.0, 10.0, 0.2]]);
        assert_eq!(beads[1].points, vec![[30.0, 10.0, 0.2], [40.0, 10.0, 0.2]]);
    }

    #[test]
    fn nothing_known_means_nothing_drawn() {
        assert!(read(b"G1 X10 E5\nG1 E5\n", BED, 1.75).is_empty());
        assert!(read(b"", BED, 1.75).is_empty());
        assert!(read(b"G1 X1 Y1 Z0.2 Enan\n", BED, 1.75).is_empty());
    }
}
