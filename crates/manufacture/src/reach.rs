//! The finished G-code read back the way a printer would move: every absolute X, Y and Z a
//! `G0`/`G1`/`G2`/`G3` names must lie inside the bed and between the bed and the printable
//! height. This catches a start or end template that would send the head somewhere the
//! engine's own toolpath check never sees, since templates are not toolpaths.

/// The first moves outside the volume, as `line N: ...` texts, and how many there were.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outside {
    pub count: usize,
    pub first: Vec<String>,
}

/// Checks `gcode` against the printable rectangle `[min_x, min_y, max_x, max_y]` and `height`,
/// in millimetres. `G91` moves are tracked from the last known position; a relative move with no
/// known position is skipped, not refused.
pub fn check(gcode: &[u8], bed: [f64; 4], height: f64) -> Outside {
    const TOLERANCE: f64 = 0.01;
    let mut out = Outside::default();
    let mut relative = false;
    let mut pos: [Option<f64>; 3] = [None; 3];
    for (n, raw) in gcode.split(|&b| b == b'\n').enumerate() {
        let line = String::from_utf8_lossy(raw);
        let code = line.split(';').next().unwrap_or("").trim();
        let mut words = code.split_ascii_whitespace();
        let Some(cmd) = words.next() else { continue };
        match cmd {
            "G90" => relative = false,
            "G91" => relative = true,
            "G28" => pos = [None; 3],
            "G92" => {
                // Sets the position the firmware believes; only the axes named change.
                for w in words {
                    if let Some(i) = axis(w) {
                        pos[i] = number(w);
                    }
                }
            }
            "G0" | "G1" | "G2" | "G3" => {
                for w in words {
                    let Some(i) = axis(w) else { continue };
                    let Some(v) = number(w) else {
                        out.note(n, format!("`{w}` is not a number"));
                        continue;
                    };
                    let next = if relative { pos[i].map(|p| p + v) } else { Some(v) };
                    let Some(next) = next else { continue };
                    pos[i] = Some(next);
                    let (lo, hi) = match i {
                        0 => (bed[0], bed[2]),
                        1 => (bed[1], bed[3]),
                        _ => (0.0, height),
                    };
                    if next < lo - TOLERANCE || next > hi + TOLERANCE {
                        out.note(n, format!("{} = {next} is outside {lo} to {hi} mm", ['X', 'Y', 'Z'][i]));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

impl Outside {
    fn note(&mut self, line: usize, what: String) {
        self.count += 1;
        if self.first.len() < 10 {
            self.first.push(format!("line {}: {what}", line + 1));
        }
    }
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

    #[test]
    fn moves_inside_the_volume_pass_and_moves_outside_are_named() {
        let bed = [0.0, 0.0, 305.0, 305.0];
        let ok = b"G90\nG0 X10 Y20 Z5 F6000\nG1 X305 Y305 E1\nG1 Z280\nG91\nG1 Z-5\nG90\nG1 X0.005 ; a hair past zero\n";
        assert_eq!(check(ok, bed, 280.0), Outside::default());
        let bad = b"G90\nG1 X10 Y10 Z0.2\nG1 X400 ; off the bed\nG1 Z-1\nG91\nG1 Z300\nG1 Xnan\n";
        let r = check(bad, bed, 280.0);
        assert_eq!(r.count, 4, "{r:?}");
        assert!(r.first[0].starts_with("line 3: X = 400"), "{r:?}");
        assert!(r.first[1].starts_with("line 4: Z = -1"), "{r:?}");
        assert!(r.first[2].starts_with("line 6: Z = 299"), "{r:?}");
        assert!(r.first[3].contains("not a number"), "{r:?}");
    }

    #[test]
    fn a_relative_move_before_any_position_is_known_is_not_judged() {
        let r = check(b"G91\nG1 Z2\nG28\nG91\nG0 X-5\n", [0.0, 0.0, 100.0, 100.0], 100.0);
        assert_eq!(r, Outside::default());
    }
}
