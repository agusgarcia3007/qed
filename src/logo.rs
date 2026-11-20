//! Animated logo: a rotating tesseract (4D hypercube) drawn in braille, next to `qed`.
//! A quarter turn in the XW/ZW planes is a full cycle for a tesseract: the inner cube
//! becomes the outer one. "q.e.d." closes a proof; its mark is the tombstone ∎.

use std::io::Write;
use std::time::Duration;

const ART: [&str; 5] = [
    "                █",
    "▄▀▀▀▄ ▄▀▀▀▄ ▄▀▀▀█",
    "█   █ █▀▀▀▀ █   █",
    "▀▄▄▄█ ▀▄▄▄▄ ▀▄▄▄█",
    "    █            ",
];
const ART_W: usize = 17;
const SUB: &str = "quod erat demonstrandum";
// Braille canvas: COLS×ROWS cells = (2·COLS)×(4·ROWS) dots (dots are ~square).
const COLS: usize = 24;
const ROWS: usize = 10;
const GAP: usize = 3;
const ART_TOP: usize = 2;
const SUB_ROW: usize = 8;
const FRAMES: usize = 26;
// 256-color teal → violet gradient; gold for the tombstone.
const PALETTE: [u8; 7] = [37, 38, 39, 63, 99, 135, 171];
const GOLD: u8 = 220;

type P2 = (f64, f64);

/// The 16 vertices of the unit tesseract and its 32 edges (vertices differing in one bit).
fn edges() -> Vec<(usize, usize)> {
    let mut e = Vec::new();
    for a in 0..16usize {
        for bit in 0..4 {
            let b = a ^ (1 << bit);
            if a < b {
                e.push((a, b));
            }
        }
    }
    e
}

fn project(theta: f64) -> Vec<P2> {
    (0..16)
        .map(|i| {
            let c = |bit: usize| if i >> bit & 1 == 1 { 1.0 } else { -1.0 };
            let (x, y, z, w) = (c(0), c(1), c(2), c(3));
            // 4D rotation in the XW and ZW planes.
            let (s, co) = theta.sin_cos();
            let (x, w) = (x * co - w * s, x * s + w * co);
            let (z, w) = (z * co - w * s, z * s + w * co);
            // 4D -> 3D perspective.
            let k = 2.0 / (3.0 - w);
            let (x, y, z) = (x * k, y * k, z * k);
            // Fixed 3D tilt so it reads as depth, then 3D -> 2D perspective.
            let (sy, cy) = 0.55f64.sin_cos();
            let (x, z) = (x * cy + z * sy, -x * sy + z * cy);
            let (sx, cx) = 0.35f64.sin_cos();
            let (y, z) = (y * cx - z * sx, y * sx + z * cx);
            let k2 = 4.0 / (6.0 - z);
            (x * k2, y * k2)
        })
        .collect()
}

/// Rasterize all frames into braille cells; returns frames[f][row][col] as dot bitmasks.
fn frames() -> Vec<Vec<Vec<u8>>> {
    let pts: Vec<Vec<P2>> = (0..FRAMES)
        .map(|f| project(std::f64::consts::FRAC_PI_2 * f as f64 / (FRAMES - 1) as f64))
        .collect();
    // One scale for every frame so the figure doesn't jitter.
    let m = pts
        .iter()
        .flatten()
        .fold(0.0f64, |m, p| m.max(p.0.abs()).max(p.1.abs()));
    let (dw, dh) = ((COLS * 2) as f64, (ROWS * 4) as f64);
    let scale = ((dw - 2.0) / 2.0).min((dh - 2.0) / 2.0) / m;
    let es = edges();
    pts.iter()
        .map(|p| {
            let mut cells = vec![vec![0u8; COLS]; ROWS];
            let to = |q: P2| ((dw / 2.0 + q.0 * scale), (dh / 2.0 - q.1 * scale));
            for &(a, b) in &es {
                let (x0, y0) = to(p[a]);
                let (x1, y1) = to(p[b]);
                let n = ((x1 - x0).abs().max((y1 - y0).abs()).ceil() as usize).max(1);
                for i in 0..=n {
                    let t = i as f64 / n as f64;
                    let (x, y) = ((x0 + (x1 - x0) * t).round(), (y0 + (y1 - y0) * t).round());
                    if x < 0.0 || y < 0.0 || x >= dw || y >= dh {
                        continue;
                    }
                    let (x, y) = (x as usize, y as usize);
                    const BITS: [[u8; 2]; 4] =
                        [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]];
                    cells[y / 4][x / 2] |= BITS[y % 4][x % 2];
                }
            }
            cells
        })
        .collect()
}

fn render(cells: &[Vec<u8>], art_cols: usize, sub: usize) -> String {
    let mut out = String::new();
    for (r, row) in cells.iter().enumerate() {
        out.push_str("\x1b[2K ");
        for (c, &b) in row.iter().enumerate() {
            if b == 0 {
                out.push(' ');
            } else {
                let ch = char::from_u32(0x2800 + b as u32).unwrap_or(' ');
                out.push_str(&format!(
                    "\x1b[38;5;{}m{}",
                    PALETTE[c * PALETTE.len() / COLS],
                    ch
                ));
            }
        }
        out.push_str("\x1b[0m");
        out.push_str(&" ".repeat(GAP));
        if (ART_TOP..ART_TOP + ART.len()).contains(&r) {
            for (c, ch) in ART[r - ART_TOP].chars().enumerate() {
                if c < art_cols && ch != ' ' {
                    out.push_str(&format!(
                        "\x1b[1;38;5;{}m{}",
                        PALETTE[c * PALETTE.len() / ART_W],
                        ch
                    ));
                } else {
                    out.push(' ');
                }
            }
            out.push_str("\x1b[0m");
        } else if r == SUB_ROW && sub > 0 {
            let s: String = SUB.chars().take(sub).collect();
            out.push_str(&format!("\x1b[2;3m{}\x1b[0m", s));
            if sub >= SUB.len() {
                out.push_str(&format!(" \x1b[38;5;{}m∎\x1b[0m", GOLD));
            }
        }
        out.push('\n');
    }
    out
}

/// Plays the animation (~0.7 s). Only call when stdout is a terminal.
pub fn play() {
    let fs = frames();
    let mut o = std::io::stdout().lock();
    let _ = write!(o, "\x1b[?25l");
    let n = fs.len();
    for (i, cells) in fs.iter().enumerate() {
        if i > 0 {
            let _ = write!(o, "\x1b[{}A", ROWS);
        }
        // Letters are revealed during the first half, the Latin types out in the second.
        let art = (ART_W * 2 * (i + 1) / n).min(ART_W);
        let sub = if i * 2 >= n {
            SUB.len() * (2 * i + 2 - n) / n
        } else {
            0
        };
        let _ = write!(o, "{}", render(cells, art, sub.min(SUB.len())));
        let _ = o.flush();
        std::thread::sleep(Duration::from_millis(26));
    }
    let _ = write!(o, "\x1b[?25h");
    let _ = o.flush();
}

/// The startup animation is opt-in (QED_LOGO=1): opening qed never waits ~0.7 s on it.
pub fn enabled() -> bool {
    // SAFETY: isatty only inspects the descriptor.
    std::env::var_os("QED_LOGO").is_some() && unsafe { libc::isatty(1) == 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tesseract_has_32_edges_and_fits() {
        assert_eq!(edges().len(), 32);
        let fs = frames();
        assert_eq!(fs.len(), FRAMES);
        // Every frame draws something and the last frame shows the full text.
        assert!(fs.iter().all(|f| f.iter().flatten().any(|&b| b != 0)));
        let last = render(&fs[FRAMES - 1], ART_W, SUB.len());
        assert!(last.contains(SUB) && last.contains('∎'));
        assert!(ART.iter().all(|r| r.chars().count() == ART_W));
    }
}
