//! Knight Rider scanner spinner for the live status line.
//!
//! Ported from opencode's `ui/spinner.ts`: a bidirectional 8-cell scanner
//! with an alpha-fading trail, per-cell color, and hold frames at each end.
//! Because we render to ANSI (no alpha compositing), trail and inactive
//! colors are pre-blended against the terminal background to opaque RGB.
//! The accent color is supplied by the auto-generating theme palette so the
//! scanner matches the user's terminal.

use crate::palette::Rgb;

// ── defaults ──────────────────────────────────────────────────────────────

const WIDTH: usize = 8;
const TRAIL_STEPS: usize = 6;
const HOLD_START: usize = 30;
const HOLD_END: usize = 9;
const INACTIVE_FACTOR: f64 = 0.2;
const MIN_ALPHA: f64 = 0.0;

// Blocks-style glyphs (one cell each).
const GLYPH_ACTIVE: &str = "\u{25a0}"; // ■
const GLYPH_INACTIVE: &str = "\u{2b1d}"; // ⬝

const RESET: &str = "\x1b[0m";

// ── scanner state ──────────────────────────────────────────────────────────

struct ScannerState {
    active_position: usize,
    is_holding: bool,
    hold_progress: usize,
    hold_total: usize,
    movement_progress: usize,
    movement_total: usize,
    is_moving_forward: bool,
}

/// Total frames in one bidirectional sweep cycle:
/// forward (WIDTH) + hold-end (HOLD_END) + backward (WIDTH-1) + hold-start (HOLD_START).
const fn total_frames() -> usize {
    WIDTH + HOLD_END + (WIDTH - 1) + HOLD_START
}

fn get_scanner_state(frame_index: usize) -> ScannerState {
    let forward = WIDTH;
    let hold_end = HOLD_END;
    let backward = WIDTH - 1;

    if frame_index < forward {
        // Moving forward (L→R).
        ScannerState {
            active_position: frame_index,
            is_holding: false,
            hold_progress: 0,
            hold_total: 0,
            movement_progress: frame_index,
            movement_total: forward,
            is_moving_forward: true,
        }
    } else if frame_index < forward + hold_end {
        // Holding at the right end.
        ScannerState {
            active_position: WIDTH - 1,
            is_holding: true,
            hold_progress: frame_index - forward,
            hold_total: hold_end,
            movement_progress: 0,
            movement_total: 0,
            is_moving_forward: true,
        }
    } else if frame_index < forward + hold_end + backward {
        // Moving backward (R→L).
        let bi = frame_index - forward - hold_end;
        ScannerState {
            active_position: WIDTH - 2 - bi,
            is_holding: false,
            hold_progress: 0,
            hold_total: 0,
            movement_progress: bi,
            movement_total: backward,
            is_moving_forward: false,
        }
    } else {
        // Holding at the left end.
        ScannerState {
            active_position: 0,
            is_holding: true,
            hold_progress: frame_index - forward - hold_end - backward,
            hold_total: HOLD_START,
            movement_progress: 0,
            movement_total: 0,
            is_moving_forward: false,
        }
    }
}

/// Returns the trail-color index for `(frame, char)`, or -1 if the cell is
/// inactive.  Mirrors `calculateColorIndex` from opencode.
fn calculate_color_index(char_index: usize, state: &ScannerState) -> i32 {
    let dir_dist = if state.is_moving_forward {
        state.active_position as i32 - char_index as i32
    } else {
        char_index as i32 - state.active_position as i32
    };

    if state.is_holding {
        return dir_dist + state.hold_progress as i32;
    }

    if dir_dist > 0 && (dir_dist as usize) < TRAIL_STEPS {
        dir_dist
    } else if dir_dist == 0 {
        0
    } else {
        -1
    }
}

// ── color derivation ───────────────────────────────────────────────────────

/// Blend `fg` over `bg` at the given alpha (0..1), returning opaque RGB.
fn blend(fg: f64, bg: f64, alpha: f64) -> u8 {
    (fg * alpha + bg * (1.0 - alpha)).round().clamp(0.0, 255.0) as u8
}

/// Derive trail colors from a single bright color.
///
/// Each step gets a brightness factor (1.0 for the lead, 1.15 bloom on step 1,
/// 1.0 after) and an exponential alpha falloff.  Alpha is pre-blended against
/// `background` so the resulting colors are opaque RGB suitable for ANSI.
fn derive_trail_colors(bright: Rgb, bg: Rgb, steps: usize) -> Vec<Rgb> {
    (0..steps)
        .map(|i| {
            let (alpha, brightness) = if i == 0 {
                (1.0, 1.0)
            } else if i == 1 {
                (0.9, 1.15)
            } else {
                (0.65_f64.powi((i - 1) as i32), 1.0)
            };
            let r = (bright.0 as f64 * brightness).min(255.0);
            let g = (bright.1 as f64 * brightness).min(255.0);
            let b = (bright.2 as f64 * brightness).min(255.0);
            Rgb(
                blend(r, bg.0 as f64, alpha),
                blend(g, bg.1 as f64, alpha),
                blend(b, bg.2 as f64, alpha),
            )
        })
        .collect()
}

/// Inactive-dot color: same hue as the bright color at a low alpha, blended
/// against the background.
#[allow(dead_code)] // used in tests
fn derive_inactive(bright: Rgb, bg: Rgb, factor: f64) -> Rgb {
    Rgb(
        blend(bright.0 as f64, bg.0 as f64, factor),
        blend(bright.1 as f64, bg.1 as f64, factor),
        blend(bright.2 as f64, bg.2 as f64, factor),
    )
}

// ── frame pre-computation ─────────────────────────────────────────────────

/// Build all frames as pre-colored ANSI strings.  Each glyph carries its own
/// `\x1b[38;2;R;G;Bm` escape; the string ends with RESET.
fn build_frames(accent: Rgb, background: Rgb) -> Vec<String> {
    let trail = derive_trail_colors(accent, background, TRAIL_STEPS);

    (0..total_frames())
        .map(|fi| {
            let state = get_scanner_state(fi);
            let mut frame = String::with_capacity(WIDTH * 20);
            for ci in 0..WIDTH {
                let idx = calculate_color_index(ci, &state);

                // Fade factor only applies to *inactive* dots.
                let fade = if state.is_holding && state.hold_total > 0 {
                    let p = (state.hold_progress as f64 / state.hold_total as f64).min(1.0);
                    MIN_ALPHA.max(1.0 - p * (1.0 - MIN_ALPHA))
                } else if !state.is_holding && state.movement_total > 0 {
                    let denom = (state.movement_total as f64 - 1.0).max(1.0);
                    let p = (state.movement_progress as f64 / denom).min(1.0);
                    MIN_ALPHA + p * (1.0 - MIN_ALPHA)
                } else {
                    1.0
                };

                if idx >= 0 && (idx as usize) < trail.len() {
                    // Active / trail cell — full trail color (no fade).
                    let c = trail[idx as usize];
                    frame.push_str(&format!(
                        "\x1b[38;2;{};{};{}m{}",
                        c.0, c.1, c.2, GLYPH_ACTIVE
                    ));
                } else {
                    // Inactive cell — apply fade by blending inactive against bg.
                    let a = INACTIVE_FACTOR * fade;
                    let c = Rgb(
                        blend(accent.0 as f64, background.0 as f64, a),
                        blend(accent.1 as f64, background.1 as f64, a),
                        blend(accent.2 as f64, background.2 as f64, a),
                    );
                    frame.push_str(&format!(
                        "\x1b[38;2;{};{};{}m{}",
                        c.0, c.1, c.2, GLYPH_INACTIVE
                    ));
                }
            }
            frame.push_str(RESET);
            frame
        })
        .collect()
}

// ── public API ────────────────────────────────────────────────────────────

pub struct Spinner {
    idx: usize,
    frames: Vec<String>,
}

impl Spinner {
    /// Create a Knight Rider spinner using `accent` as the bright scanner color,
    /// pre-blended against `background`.
    pub fn new(accent: Rgb, background: Rgb) -> Self {
        Self {
            idx: 0,
            frames: build_frames(accent, background),
        }
    }

    /// Current frame as a fully-colored ANSI string.
    pub fn frame(&self) -> &str {
        &self.frames[self.idx]
    }

    /// Advance the frame by one step and return the current frame.
    ///
    /// This mirrors opencode's `<spinner interval={40}>` approach: the
    /// caller drives a fixed interval timer (`tokio::time::interval(40ms)`)
    /// and calls `tick()` once per fire.  The spinner simply increments a
    /// frame counter — no wall-clock tracking, no drift.  This is the same
    /// pattern as `setInterval(advanceByOne, 40)` in opencode's
    /// `opentui-spinner`.
    pub fn tick(&mut self) -> &str {
        self.idx = (self.idx + 1) % self.frames.len();
        &self.frames[self.idx]
    }
}

impl Default for Spinner {
    fn default() -> Self {
        Self::new(Rgb(100, 200, 220), Rgb(20, 20, 26))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn total_frame_count_matches_opencode_defaults() {
        // width(8) + holdEnd(9) + backward(7) + holdStart(30) = 54
        assert_eq!(total_frames(), 54);
    }

    #[test]
    fn frames_are_non_empty_and_ansi() {
        let s = Spinner::default();
        assert_eq!(s.frames.len(), 54);
        let f = s.frame();
        assert!(f.starts_with("\x1b[38;2;"));
        assert!(f.ends_with(RESET));
    }

    #[test]
    fn frame_zero_is_lead_at_left() {
        let s = Spinner::default();
        // At frame 0 the scanner is at position 0 (forward, first step).
        // Cell 0 should be active (■), cell 7 inactive (⬝).
        let f0 = &s.frames[0];
        // The first glyph is ■ (after the SGR escape for the active lead).
        assert!(f0.contains(GLYPH_ACTIVE));
        assert!(f0.contains(GLYPH_INACTIVE));
    }

    #[test]
    fn bidirectional_sweep_visits_all_positions() {
        let _s = Spinner::default();
        // Collect the active position at each forward frame.
        let positions: Vec<usize> = (0..WIDTH)
            .map(|fi| {
                let st = get_scanner_state(fi);
                st.active_position
            })
            .collect();
        assert_eq!(positions, (0..WIDTH).collect::<Vec<_>>());
    }

    #[test]
    fn tick_advances_frames() {
        let mut s = Spinner::default();
        let initial = s.idx;
        s.tick();
        assert_ne!(s.idx, initial);
    }

    #[test]
    fn trail_colors_blend_against_background() {
        let bright = Rgb(255, 0, 0);
        let bg = Rgb(0, 0, 0);
        let trail = derive_trail_colors(bright, bg, 6);
        // Lead color should be fully bright (alpha 1.0 → pure RGB).
        assert_eq!(trail[0], Rgb(255, 0, 0));
        // Step 1 has bloom factor 1.15 → R=255*1.15 clamped to 255, then
        // blended at alpha 0.9 → 255*0.9 = 229.5 → 230.
        assert_eq!(trail[1].0, 230);
        // Later steps should be dimmer than the lead.
        assert!(trail[5].0 < trail[0].0);
    }

    #[test]
    fn inactive_color_is_dimmer_than_trail() {
        let bright = Rgb(255, 100, 100);
        let bg = Rgb(20, 20, 26);
        let inactive = derive_inactive(bright, bg, INACTIVE_FACTOR);
        let trail = derive_trail_colors(bright, bg, TRAIL_STEPS);
        // Inactive (factor 0.2) should be much dimmer than the lead trail color.
        assert!(inactive.0 < trail[0].0);
    }

    #[test]
    fn calculate_color_index_forward_trail() {
        // Forward: active_position=3, so trail is at positions 2,1,0.
        let state = get_scanner_state(3); // forward, position 3
        assert_eq!(calculate_color_index(3, &state), 0); // lead
        assert_eq!(calculate_color_index(2, &state), 1); // trail 1
        assert_eq!(calculate_color_index(0, &state), 3); // trail 3
        assert_eq!(calculate_color_index(4, &state), -1); // ahead → inactive
    }

    #[test]
    fn calculate_color_index_backward_trail() {
        // Backward frame: active_position goes from WIDTH-2 down to 0.
        // The first backward frame (fi=WIDTH+HOLD_END) has active_position = WIDTH-2.
        let fi = WIDTH + HOLD_END;
        let state = get_scanner_state(fi);
        assert_eq!(flexible_position(&state), WIDTH - 2);
        assert_eq!(calculate_color_index(WIDTH - 2, &state), 0); // lead
        assert_eq!(calculate_color_index(WIDTH - 1, &state), 1); // trail (to the right)
        assert_eq!(calculate_color_index(0, &state), -1); // behind → inactive
    }

    fn flexible_position(s: &ScannerState) -> usize {
        s.active_position
    }

    #[test]
    fn hold_frames_shift_trail_offscreen() {
        // During hold at end, holdProgress grows, pushing trail color indices up.
        let s0 = get_scanner_state(WIDTH); // first hold-end frame
        let s5 = get_scanner_state(WIDTH + 5); // 5th hold-end frame
        // At position WIDTH-2 (one behind the lead):
        let idx0 = calculate_color_index(WIDTH - 2, &s0);
        let idx5 = calculate_color_index(WIDTH - 2, &s5);
        // The color index should increase with hold progress.
        assert!(idx5 > idx0, "{idx5} should be > {idx0}");
    }
}
