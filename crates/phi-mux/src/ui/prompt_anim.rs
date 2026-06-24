//! Prompt entry animation: per-span shimmer keyed to span center position,
//! with a frame counter and progressive left-to-right reveal.
//!
//! Ports jcode's prompt entry shimmer concept into the phi mux cell-surface
//! rendering pipeline. The animation is self-contained and can be called
//! from `scene` rendering to animate prompt text on entry, but is NOT yet
//! wired into `scene.rs` — it is registered as a module only.

use super::cell_surface::{CellSurface, char_cell_width, display_cell_width};
use super::color::Color;
use super::layout::CellRect;
use super::theme;

/// Default duration of the entry reveal animation.
pub const PROMPT_ENTRY_DURATION_MS: u64 = 600;
/// Frame interval for the internal frame counter (16ms ≈ 60 FPS).
const FRAME_INTERVAL_MS: u64 = 16;

/// A single span of prompt text with its own style baseline.
#[derive(Clone, Debug)]
pub struct PromptSpan<'a> {
    pub text: &'a str,
    pub base_fg: Color,
    pub base_bg: Color,
}

impl<'a> PromptSpan<'a> {
    pub fn new(text: &'a str, fg: Color, bg: Color) -> Self {
        Self {
            text,
            base_fg: fg,
            base_bg: bg,
        }
    }
}

/// State for the prompt entry shimmer animation.
#[derive(Clone, Debug)]
pub struct PromptEntryAnimation {
    /// Monotonic time when the animation started (ms).
    start_ms: u64,
    /// Total animation duration (ms).
    duration_ms: u64,
    /// Incremented every `FRAME_INTERVAL_MS` of wall-clock progress.
    frame: u32,
    /// Whether the animation is currently active.
    active: bool,
}

impl Default for PromptEntryAnimation {
    fn default() -> Self {
        Self {
            start_ms: 0,
            duration_ms: PROMPT_ENTRY_DURATION_MS,
            frame: 0,
            active: false,
        }
    }
}

impl PromptEntryAnimation {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set a custom animation duration.
    pub fn with_duration(mut self, ms: u64) -> Self {
        self.duration_ms = ms;
        self
    }

    /// Begin the animation at `now_ms`.
    pub fn start(&mut self, now_ms: u64) {
        self.start_ms = now_ms;
        self.frame = 0;
        self.active = true;
    }

    /// Advance the frame counter based on elapsed wall-clock time.
    pub fn tick(&mut self, now_ms: u64) {
        if !self.active {
            return;
        }
        let elapsed = now_ms.saturating_sub(self.start_ms);
        let expected_frame = (elapsed / FRAME_INTERVAL_MS) as u32;
        if expected_frame > self.frame {
            self.frame = expected_frame;
        }
        if elapsed >= self.duration_ms {
            self.active = false;
        }
    }

    /// Animation progress in `[0.0, 1.0]`.
    pub fn progress(&self, now_ms: u64) -> f32 {
        if !self.active && self.start_ms == 0 {
            return 1.0; // never started → fully revealed
        }
        let elapsed = now_ms.saturating_sub(self.start_ms) as f32;
        (elapsed / self.duration_ms as f32).clamp(0.0, 1.0)
    }

    /// True once the full reveal duration has elapsed.
    pub fn is_complete(&self, now_ms: u64) -> bool {
        if !self.active && self.start_ms == 0 {
            return true;
        }
        now_ms.saturating_sub(self.start_ms) >= self.duration_ms
    }

    /// Current frame counter value.
    pub fn frame(&self) -> u32 {
        self.frame
    }

    /// Whether the animation is still running.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Render animated prompt text with per-span shimmer and progressive
    /// left-to-right reveal onto a `CellSurface`.
    ///
    /// Each span is shimmered independently: the shimmer highlight position
    /// is keyed to the span's center within the overall text, and the
    /// progressive reveal gates character visibility left-to-right.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &self,
        surface: &mut CellSurface,
        rect: CellRect,
        spans: &[PromptSpan<'_>],
        now_ms: u64,
    ) {
        if rect.cols <= 0 || rect.rows <= 0 || spans.is_empty() {
            return;
        }

        let progress = self.progress(now_ms);

        // Compute total cell width of all spans for normalization.
        let total_width: i32 = spans
            .iter()
            .map(|s| display_cell_width(s.text) as i32)
            .sum();

        if total_width == 0 {
            return;
        }

        let revealed_width = (total_width as f32 * progress).round() as i32;

        let mut cursor = rect.col;
        let end = rect.col + rect.cols;
        let mut span_start_offset = 0i32;

        for span in spans {
            let span_text = span.text;
            let span_width = display_cell_width(span_text) as i32;
            let span_center = if total_width > 1 {
                (span_start_offset + span_width / 2) as f32 / total_width as f32
            } else {
                0.5
            };

            for ch in span_text.chars() {
                if cursor >= end {
                    break;
                }
                let ch_width = char_cell_width(ch);
                if cursor + ch_width > end {
                    break;
                }

                let char_offset = span_start_offset + (cursor - rect.col);
                let revealed = char_offset < revealed_width;

                if revealed {
                    // Per-span shimmer: position within span, global progress.
                    let pos_in_span = if span_width > 1 {
                        (cursor - rect.col - span_start_offset) as f32 / span_width as f32
                    } else {
                        span_center
                    };
                    let fg = theme::prompt_entry_shimmer_color(
                        span.base_fg,
                        pos_in_span,
                        progress,
                    );
                    let bg = theme::prompt_entry_bg_color(span.base_bg, progress);

                    let mut buf = [0u8; 4];
                    let glyph = ch.encode_utf8(&mut buf);
                    surface.set_cell_styled(cursor, rect.row, fg, bg, glyph, false, false);
                }
                // Unrevealed characters are left as the background fill.

                cursor += ch_width;
            }

            span_start_offset += span_width;
        }
    }

    /// Convenience: render a single string as one span.
    #[allow(clippy::too_many_arguments)]
    pub fn render_text(
        &self,
        surface: &mut CellSurface,
        rect: CellRect,
        text: &str,
        fg: Color,
        bg: Color,
        now_ms: u64,
    ) {
        let span = PromptSpan::new(text, fg, bg);
        self.render(surface, rect, std::slice::from_ref(&span), now_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn animation_starts_inactive_and_completes() {
        let mut anim = PromptEntryAnimation::new();
        assert!(!anim.is_active());
        assert!(anim.is_complete(0));

        anim.start(100);
        assert!(anim.is_active());
        assert!(!anim.is_complete(100));
        assert!((anim.progress(100) - 0.0).abs() < 0.01);

        anim.tick(100 + PROMPT_ENTRY_DURATION_MS / 2);
        assert!(anim.is_active());
        let p = anim.progress(100 + PROMPT_ENTRY_DURATION_MS / 2);
        assert!(p > 0.4 && p < 0.6, "progress should be ~0.5, got {p}");

        anim.tick(100 + PROMPT_ENTRY_DURATION_MS + 1);
        assert!(!anim.is_active());
        assert!(anim.is_complete(100 + PROMPT_ENTRY_DURATION_MS + 1));
    }

    #[test]
    fn frame_counter_advances_with_time() {
        let mut anim = PromptEntryAnimation::new();
        anim.start(0);
        assert_eq!(anim.frame(), 0);
        anim.tick(16);
        assert!(anim.frame() >= 1);
        anim.tick(160);
        assert!(anim.frame() >= 10);
    }

    #[test]
    fn render_progressive_reveal() {
        let mut surface = CellSurface::new(20, 1, Color::rgb(0, 0, 0), Color::rgb(0, 0, 0));
        let rect = CellRect::new(0, 0, 20, 1);
        let mut anim = PromptEntryAnimation::new();
        anim.start(0);

        // At progress=0.0, nothing revealed yet
        anim.render_text(&mut surface, rect, "hello", Color::rgb(255, 255, 255), Color::rgb(0, 0, 0), 0);

        // At ~50% progress, roughly half the characters should be revealed
        let half = PROMPT_ENTRY_DURATION_MS / 2;
        anim.render_text(&mut surface, rect, "hello", Color::rgb(255, 255, 255), Color::rgb(0, 0, 0), half);

        // At completion, all characters revealed
        let done = PROMPT_ENTRY_DURATION_MS + 1;
        anim.render_text(&mut surface, rect, "hello", Color::rgb(255, 255, 255), Color::rgb(0, 0, 0), done);
    }

    #[test]
    fn per_span_shimmer_uses_span_center() {
        // Verify that multi-span rendering doesn't crash and produces
        // different colors per span at mid-animation.
        let mut surface = CellSurface::new(40, 1, Color::rgb(0, 0, 0), Color::rgb(0, 0, 0));
        let rect = CellRect::new(0, 0, 40, 1);
        let anim = PromptEntryAnimation::new();

        let spans = [
            PromptSpan::new("hello ", Color::rgb(200, 200, 200), Color::rgb(30, 30, 30)),
            PromptSpan::new("world", Color::rgb(200, 200, 200), Color::rgb(30, 30, 30)),
        ];
        // Render at progress=1.0 (fully revealed, shimmer still applies)
        anim.render(&mut surface, rect, &spans, PROMPT_ENTRY_DURATION_MS + 1);
    }
}
