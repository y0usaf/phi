//! Simple braille-dot spinner for the live status line.
//!
//! A lightweight 10-frame braille spinner with no color blending, no trail
//! effects, and no pre-computed frames — just a static array of glyphs.
//!
//! This is the **single source of truth** for spinner frames and timing
//! across all of phi (session-tui, mux sidebar, grid theme). Every consumer
//! should use [`SPINNER_FRAMES`], [`SPINNER_FRAME_MS`], and
//! [`spinner_frame_at`] rather than defining its own copy.

use std::time::Instant;

/// Braille dot spinner frames.
pub const SPINNER_FRAMES: [&str; 10] = [
    "\u{280B}", "\u{2819}", "\u{2839}", "\u{2838}", "\u{283C}", "\u{2834}", "\u{2826}",
    "\u{2827}", "\u{2807}", "\u{280F}",
];

/// Duration each spinner frame is displayed, in milliseconds.
///
/// At 80 ms/frame the 10-frame sequence completes one full cycle in 800 ms
/// (12.5 FPS), which reads as a smooth, unhurried spin without flickering.
pub const SPINNER_FRAME_MS: u64 = 80;

/// Compute the spinner frame index for a given millisecond timestamp or
/// elapsed duration. Any monotonically increasing millisecond value works
/// (wall-clock `now_ms`, monotonic elapsed, etc.) — only the value modulo
/// `SPINNER_FRAME_MS * SPINNER_FRAMES.len()` determines the phase.
pub fn spinner_frame_index(ms: u64) -> usize {
    ((ms / SPINNER_FRAME_MS) as usize) % SPINNER_FRAMES.len()
}

/// Return the spinner glyph for a given millisecond timestamp or elapsed
/// duration.
pub fn spinner_frame_at(ms: u64) -> &'static str {
    SPINNER_FRAMES[spinner_frame_index(ms)]
}

// ── "equalizer" animation (VU-meter-style bars ▁▂▃…█) ───────────
//
// A 5-bar VU-meter animation where each bar rises and falls independently
// via a phase-shifted sine wave, mimicking the @nerisma/pi-input-revamp
// equalizer. Accompanied by cycling "thinking" phrases.

/// Block characters used for the equalizer bars, from smallest to tallest.
pub const EQUALIZER_BARS: [&str; 8] = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];

/// Number of bars in the equalizer cluster.
pub const EQUALIZER_BAR_COUNT: usize = 5;

/// Sine-wave divisor controlling the animation speed (ms). Lower = faster.
/// Matches the pi-input-revamp rhythm (≈6.7 Hz oscillation per bar).
const EQUALIZER_SINE_DIV: f64 = 150.0;

/// Phase offset between adjacent bars (radians). Creates the wave-like
/// cascade across the cluster.
const EQUALIZER_PHASE_SHIFT: f64 = 0.9;

/// Render the equalizer glyph cluster as a plain string (no colors).
///
/// Returns 5 block characters whose heights follow phase-shifted sine waves,
/// producing a "dancing bars" VU-meter effect. `elapsed_ms` is the wall-clock
/// elapsed time since the animation started.
pub fn equalizer_glyphs(elapsed_ms: u64) -> String {
    let t = elapsed_ms as f64;
    let mut out = String::with_capacity(EQUALIZER_BAR_COUNT * 4);
    for i in 0..EQUALIZER_BAR_COUNT {
        let val = ((t / EQUALIZER_SINE_DIV + i as f64 * EQUALIZER_PHASE_SHIFT).sin() + 1.0) / 2.0; // 0..1
        let lvl = (val * (EQUALIZER_BARS.len() - 1) as f64).round() as usize;
        out.push_str(EQUALIZER_BARS[lvl.min(EQUALIZER_BARS.len() - 1)]);
    }
    out
}

/// Linearly interpolate between two RGB colors.
///
/// Port of phi-mux's `theme::fade_toward`: blends `from` toward `to` by
/// `mix`/255 on each channel.
fn fade_rgb(from: crate::palette::Rgb, to: crate::palette::Rgb, mix: u8) -> crate::palette::Rgb {
    let blend = |a: u8, b: u8| -> u8 {
        let a = u16::from(a);
        let b = u16::from(b);
        let mix = u16::from(mix);
        (((a * (255 - mix)) + (b * mix)) / 255) as u8
    };
    crate::palette::Rgb(blend(from.0, to.0), blend(from.1, to.1), blend(from.2, to.2))
}

/// Apply a per-character left-to-right RGB gradient to `text`.
///
/// Mirrors phi-mux's `render_sidebar_gradient_text`: each character's
/// foreground is a linear blend from `from` (left) to `to` (right). Emits a
/// raw `[38;2;r;g;bm` escape before each char and resets at the end.
pub fn gradient_text(text: &str, from: crate::palette::Rgb, to: crate::palette::Rgb) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return String::new();
    }
    let denom = (chars.len() - 1).max(1) as u16;
    let mut out = String::with_capacity(chars.len() * 16);
    for (i, ch) in chars.iter().enumerate() {
        let mix = ((i as u16 * 255) / denom).min(255) as u8;
        let crate::palette::Rgb(r, g, b) = fade_rgb(from, to, mix);
        out.push_str(&format!("\x1b[38;2;{r};{g};{b}m{ch}"));
    }
    out.push_str("\x1b[0m");
    out
}

/// Thinking expressions cycled while the agent is working (no active tool).
///
/// Mirrors the pi-input-revamp `THINKING_EXPRESSIONS` array. Each phrase is
/// shown for 2 seconds before cycling to the next.
pub const THINKING_EXPRESSIONS: [&str; 7] = [
    "thinking hard...",
    "pondering...",
    "cranking its neurons...",
    "cooking up an answer...",
    "crunching numbers...",
    "stirring its circuits...",
    "shuffling tokens...",
];

/// Default expression shown when a tool is actively running.
pub const DEFAULT_TOOL_EXPRESSION: &str = "doing something complex...";

/// Number of milliseconds each thinking expression is displayed before cycling.
pub const THINKING_EXPRESSION_MS: u64 = 2000;

/// Return the thinking expression for the given elapsed time.
///
/// When `tool_active` is true, always returns the default tool expression.
/// Otherwise, cycles through [`THINKING_EXPRESSIONS`] every
/// [`THINKING_EXPRESSION_MS`] milliseconds.
pub fn thinking_expression(elapsed_ms: u64, tool_active: bool) -> &'static str {
    if tool_active {
        return DEFAULT_TOOL_EXPRESSION;
    }
    let idx = (elapsed_ms / THINKING_EXPRESSION_MS) as usize % THINKING_EXPRESSIONS.len();
    THINKING_EXPRESSIONS[idx]
}

/// Wall-clock-driven braille spinner.
///
/// Stores a start [`Instant`] so that [`frame`](Self::frame) always returns
/// the correct glyph for the current wall-clock time without manual ticking.
/// Multiple `Spinner` instances (or the [`spinner_frame_at`] free function)
/// will stay in phase because they all derive from the same clock.
#[derive(Debug)]
pub struct Spinner {
    start: Instant,
}

impl Default for Spinner {
    fn default() -> Self {
        Self {
            start: Instant::now(),
        }
    }
}

impl Spinner {
    /// Construct a new spinner. Color arguments are accepted for API
    /// compatibility but ignored — the braille dots use terminal default fg.
    pub fn new(_accent: crate::palette::Rgb, _background: crate::palette::Rgb) -> Self {
        Self::default()
    }

    /// Current frame as a plain glyph string, computed from wall-clock
    /// elapsed time since construction.
    pub fn frame(&self) -> &str {
        spinner_frame_at(self.start.elapsed().as_millis() as u64)
    }

    /// No-op retained for API compatibility. The spinner is now wall-clock
    /// driven, so advancing a tick counter is unnecessary — [`frame`](Self::frame)
    /// always returns the correct glyph for the current time.
    pub fn tick(&mut self) -> &str {
        self.frame()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_non_empty() {
        let s = Spinner::default();
        assert!(!s.frame().is_empty());
    }

    #[test]
    fn frame_matches_free_function() {
        let s = Spinner::default();
        // The struct and free function should agree on the frame for the
        // same elapsed time (both derive from SPINNER_FRAMES + SPINNER_FRAME_MS).
        let elapsed = s.start.elapsed().as_millis() as u64;
        assert_eq!(s.frame(), spinner_frame_at(elapsed));
    }

    #[test]
    fn spinner_frame_at_wraps() {
        // Exactly one full cycle later → same frame as t=0.
        let cycle_ms = SPINNER_FRAME_MS * SPINNER_FRAMES.len() as u64;
        assert_eq!(spinner_frame_at(0), spinner_frame_at(cycle_ms));
        assert_eq!(spinner_frame_at(0), spinner_frame_at(cycle_ms * 7));
    }

    #[test]
    fn spinner_frame_at_advances() {
        let f0 = spinner_frame_at(0);
        let f1 = spinner_frame_at(SPINNER_FRAME_MS);
        assert_ne!(f0, f1);
    }

    #[test]
    fn equalizer_produces_5_bars() {
        let glyphs = equalizer_glyphs(0);
        assert_eq!(glyphs.chars().count(), EQUALIZER_BAR_COUNT);
    }

    #[test]
    fn equalizer_changes_over_time() {
        // At different timestamps the bar heights should differ (unless all
        // 5 sines happen to land on the same level, which is vanishingly
        // unlikely).
        let g0 = equalizer_glyphs(0);
        let g1 = equalizer_glyphs(200);
        assert_ne!(g0, g1);
    }

    #[test]
    fn equalizer_uses_only_block_chars() {
        let glyphs = equalizer_glyphs(42);
        for c in glyphs.chars() {
            assert!(
                EQUALIZER_BARS.contains(&c.to_string().as_str()),
                "unexpected char: {c}"
            );
        }
    }

    #[test]
    fn thinking_expression_cycles() {
        assert_eq!(thinking_expression(0, false), THINKING_EXPRESSIONS[0]);
        assert_eq!(
            thinking_expression(THINKING_EXPRESSION_MS, false),
            THINKING_EXPRESSIONS[1]
        );
        // Wraps around after the full cycle.
        let full_cycle = THINKING_EXPRESSION_MS * THINKING_EXPRESSIONS.len() as u64;
        assert_eq!(thinking_expression(full_cycle, false), THINKING_EXPRESSIONS[0]);
    }

    #[test]
    fn thinking_expression_tool_active_returns_default() {
        assert_eq!(thinking_expression(0, true), DEFAULT_TOOL_EXPRESSION);
        assert_eq!(thinking_expression(99999, true), DEFAULT_TOOL_EXPRESSION);
    }
}
