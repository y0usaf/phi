//! 3D ray-marching donut animation for the TUI idle indicator.
//!
//! Ported from jcode-tui-anim: uses precomputed trig lookup tables for the
//! fixed theta/phi angle sweeps and HSV-to-RGB conversion for coloring.

use std::fmt::Write;
use std::sync::OnceLock;

/// Build a `(cos, sin)` table for the angle sequence `0, step, 2*step, ...`
/// produced by the original `while a < TAU { ...; a += step }` loops.
///
/// The accumulation is replayed exactly (same `f32` start, same `f32` step,
/// same iteration bound and the same separate `cos()` / `sin()` calls in the
/// same order), so every entry is bit-identical to what the inline loop
/// computed.
fn angle_table(step: f32) -> Vec<(f32, f32)> {
    let mut table = Vec::new();
    let mut a: f32 = 0.0;
    while a < std::f32::consts::TAU {
        table.push((a.cos(), a.sin()));
        a += step;
    }
    table
}

/// Sample the 3D ray-marching donut at a given elapsed time into pixel buffers.
///
/// - `hit`:     which screen pixels are covered by the donut surface
/// - `lum_map`: surface luminance `[-1, 1]` per pixel
/// - `z_buf`:   inverse depth (`1/z`) per pixel for z-buffering
pub fn sample_donut(
    elapsed: f32,
    sw: usize,
    sh: usize,
    hit: &mut [bool],
    lum_map: &mut [f32],
    z_buf: &mut [f32],
) {
    // `theta` (step 0.04) and `phi` (step 0.014) sweep fixed angle sequences,
    // so their cos/sin are precomputed once into lookup tables.
    static THETA: OnceLock<Vec<(f32, f32)>> = OnceLock::new();
    static PHI: OnceLock<Vec<(f32, f32)>> = OnceLock::new();
    let theta_tab = THETA.get_or_init(|| angle_table(0.04));
    let phi_tab = PHI.get_or_init(|| angle_table(0.014));

    let a_rot = elapsed * 1.0;
    let b_rot = elapsed * 0.5;
    let cos_a = a_rot.cos();
    let sin_a = a_rot.sin();
    let cos_b = b_rot.cos();
    let sin_b = b_rot.sin();

    let aspect = 0.5;
    let r1 = 1.0f32;
    let r2 = 2.0f32;
    let k2 = 5.0f32;
    let k1 = (sw as f32).min(sh as f32 / aspect) * k2 * 0.35 / (r1 + r2);

    let half_w = sw as f32 / 2.0;
    let half_h = sh as f32 / 2.0;

    for &(ct, st) in theta_tab.iter() {
        let cx = r2 + r1 * ct;
        let cy = r1 * st;

        for &(cp, sp) in phi_tab.iter() {
            let x = cx * (cos_b * cp + sin_a * sin_b * sp) - cy * cos_a * sin_b;
            let y = cx * (sin_b * cp - sin_a * cos_b * sp) + cy * cos_a * cos_b;
            let z = k2 + cos_a * cx * sp + cy * sin_a;
            let ooz = 1.0 / z;

            let xp = (half_w + k1 * ooz * x) as isize;
            let yp = (half_h - k1 * ooz * y * aspect) as isize;

            let lum = cp * ct * sin_b - cos_a * ct * sp - sin_a * st
                + cos_b * (cos_a * st - ct * sin_a * sp);

            if xp >= 0 && (xp as usize) < sw && yp >= 0 && (yp as usize) < sh {
                let idx = yp as usize * sw + xp as usize;
                if ooz > z_buf[idx] {
                    z_buf[idx] = ooz;
                    lum_map[idx] = lum;
                    hit[idx] = true;
                }
            }
        }
    }
}

/// Convert HSV to RGB (h in degrees `[0, 360)`, s/v in `[0, 1]`).
pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let c = v * s;
    let h2 = h / 60.0;
    let x = c * (1.0 - (h2 % 2.0 - 1.0).abs());
    let (r1, g1, b1) = match h2 as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    (
        ((r1 + m) * 255.0) as u8,
        ((g1 + m) * 255.0) as u8,
        ((b1 + m) * 255.0) as u8,
    )
}

/// ASCII luminance ramp from dim to bright.
const LUM_RAMP: &[char] = &[' ', '.', ',', '-', '~', ':', ';', '=', '!', '*', '#', '$', '@'];

fn lum_to_char(lum: f32) -> char {
    let t = ((lum + 1.0) * 0.5).clamp(0.0, 1.0);
    let idx = (t * (LUM_RAMP.len() - 1) as f32) as usize;
    LUM_RAMP[idx.min(LUM_RAMP.len() - 1)]
}

/// Idle animation: a rotating 3D ray-marching donut.
///
/// Drop-in alternative to [`Spinner`](crate::Spinner). Unlike the single-char
/// spinner, the donut renders a multi-line frame; call [`Donut::render`] with
/// the elapsed time.
pub struct Donut {
    sw: usize,
    sh: usize,
    hit: Vec<bool>,
    lum_map: Vec<f32>,
    z_buf: Vec<f32>,
}

impl Default for Donut {
    fn default() -> Self {
        Self::new(40, 20)
    }
}

impl Donut {
    /// Create a donut renderer for a viewport of `sw` x `sh` cells.
    pub fn new(sw: usize, sh: usize) -> Self {
        let len = sw.max(1) * sh.max(1);
        Self {
            sw: sw.max(1),
            sh: sh.max(1),
            hit: vec![false; len],
            lum_map: vec![0.0; len],
            z_buf: vec![0.0; len],
        }
    }

    /// Resize internal buffers to a new viewport.
    pub fn resize(&mut self, sw: usize, sh: usize) {
        let sw = sw.max(1);
        let sh = sh.max(1);
        if self.sw == sw && self.sh == sh {
            return;
        }
        let len = sw * sh;
        self.sw = sw;
        self.sh = sh;
        self.hit = vec![false; len];
        self.lum_map = vec![0.0; len];
        self.z_buf = vec![0.0; len];
    }

    /// Current viewport dimensions.
    pub fn dimensions(&self) -> (usize, usize) {
        (self.sw, self.sh)
    }

    /// Render a single frame as a plain-text string (no color codes).
    pub fn render(&mut self, elapsed: f32) -> String {
        self.hit.fill(false);
        self.z_buf.fill(0.0);

        sample_donut(
            elapsed,
            self.sw,
            self.sh,
            &mut self.hit,
            &mut self.lum_map,
            &mut self.z_buf,
        );

        let mut out = String::with_capacity(self.sw * self.sh * 3);
        for y in 0..self.sh {
            for x in 0..self.sw {
                let idx = y * self.sw + x;
                if self.hit[idx] {
                    out.push(lum_to_char(self.lum_map[idx]));
                } else {
                    out.push(' ');
                }
            }
            out.push('\n');
        }
        out
    }

    /// Render a single frame as an ANSI true-color string.
    ///
    /// Hue cycles slowly with elapsed time; brightness modulates value via
    /// the luminance map.
    pub fn render_colored(&mut self, elapsed: f32) -> String {
        self.hit.fill(false);
        self.z_buf.fill(0.0);

        sample_donut(
            elapsed,
            self.sw,
            self.sh,
            &mut self.hit,
            &mut self.lum_map,
            &mut self.z_buf,
        );

        let hue = (elapsed * 30.0) % 360.0;
        let mut out = String::with_capacity(self.sw * self.sh * 24);
        for y in 0..self.sh {
            for x in 0..self.sw {
                let idx = y * self.sw + x;
                if self.hit[idx] {
                    let lum = self.lum_map[idx];
                    let brightness = ((lum + 1.0) * 0.5).clamp(0.0, 1.0);
                    let (r, g, b) = hsv_to_rgb(hue, 0.8, 0.3 + 0.7 * brightness);
                    let _ = write!(out, "\x1b[38;2;{r};{g};{b}m");
                    out.push(lum_to_char(lum));
                } else {
                    out.push(' ');
                }
            }
            out.push('\n');
        }
        out.push_str("\x1b[0m");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_angle_table_covers_tau() {
        let table = angle_table(0.04);
        assert!(table.len() > 100);
    }

    #[test]
    fn test_sample_donut_basic() {
        let sw = 40;
        let sh = 20;
        let len = sw * sh;
        let mut hit = vec![false; len];
        let mut lum_map = vec![0.0; len];
        let mut z_buf = vec![0.0; len];
        sample_donut(0.0, sw, sh, &mut hit, &mut lum_map, &mut z_buf);
        assert!(hit.iter().any(|&h| h));
    }

    #[test]
    fn test_donut_render() {
        let mut donut = Donut::new(40, 20);
        let frame = donut.render(0.0);
        assert!(!frame.is_empty());
        assert!(frame.contains('\n'));
    }

    #[test]
    fn test_donut_render_colored() {
        let mut donut = Donut::new(40, 20);
        let frame = donut.render_colored(1.5);
        assert!(frame.contains("\x1b[38;2;"));
        assert!(frame.ends_with("\x1b[0m"));
    }

    #[test]
    fn test_hsv_to_rgb() {
        assert_eq!(hsv_to_rgb(0.0, 1.0, 1.0), (255, 0, 0));
        assert_eq!(hsv_to_rgb(120.0, 1.0, 1.0), (0, 255, 0));
        assert_eq!(hsv_to_rgb(240.0, 1.0, 1.0), (0, 0, 255));
    }

    #[test]
    fn test_donut_resize() {
        let mut donut = Donut::new(40, 20);
        donut.resize(60, 30);
        assert_eq!(donut.dimensions(), (60, 30));
        let frame = donut.render(1.0);
        assert!(!frame.is_empty());
    }
}
