//! Viewport management for the embedded terminal pane: auto-scroll / auto-follow
//! state, scroll indicators, and an offscreen prompt preview band.
//!
//! Ported from jcode's `ui_viewport.rs` and `app/navigation.rs` scroll logic,
//! adapted to phi-mux's `CellSurface` drawing model instead of ratatui widgets.

use crate::grid::cell_surface::{CellSurface, char_cell_width, truncate_to_cells};
use crate::grid::layout::CellRect;
use crate::grid::theme;

// ---------------------------------------------------------------------------
// ViewportState
// ---------------------------------------------------------------------------

/// Tracks whether the viewport is following the live tail (auto-follow) or
/// paused because the user scrolled up (auto-scroll-paused).
///
/// In auto-follow mode the viewport snaps to the bottom on every new content
/// frame. When the user scrolls up, `auto_scroll_paused` becomes `true` and
/// the viewport stays at the user's scroll offset until they scroll back to
/// the bottom or explicitly resume.
#[derive(Clone, Debug)]
pub struct ViewportState {
    /// Current scroll offset (lines from the top of the content).
    scroll_offset: usize,
    /// `true` when the user has scrolled up and auto-follow is paused.
    auto_scroll_paused: bool,
    /// Lines of content above the viewport that are offscreen (for up indicator).
    offscreen_above: usize,
    /// Lines of content below the viewport that are offscreen (for down indicator).
    offscreen_below: usize,
    /// Resolved max scroll for the current frame.
    max_scroll: usize,
}

impl Default for ViewportState {
    fn default() -> Self {
        Self {
            scroll_offset: 0,
            auto_scroll_paused: false,
            offscreen_above: 0,
            offscreen_below: 0,
            max_scroll: 0,
        }
    }
}

impl ViewportState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Current scroll offset.
    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    /// Whether auto-follow is paused (user scrolled up).
    pub fn auto_scroll_paused(&self) -> bool {
        self.auto_scroll_paused
    }

    /// Lines offscreen above the viewport (for the up indicator).
    pub fn offscreen_above(&self) -> usize {
        self.offscreen_above
    }

    /// Lines offscreen below the viewport (for the down indicator).
    pub fn offscreen_below(&self) -> usize {
        self.offscreen_below
    }

    // -- scroll actions (ported from jcode navigation.rs) ----------------

    /// Scroll the viewport up by `amount` lines. Pauses auto-follow. Returns
    /// `true` if the scroll position changed.
    pub fn scroll_up(&mut self, amount: usize, _total_lines: usize, _viewport_height: usize) -> bool {
        if amount == 0 {
            return false;
        }
        let before = (self.scroll_offset, self.auto_scroll_paused);

        // `scroll_offset` is always an absolute position from the top. When
        // auto-follow was active, `resolve_scroll` already set it to
        // `max_scroll`, so we just subtract.
        self.scroll_offset = self.scroll_offset.saturating_sub(amount);
        self.auto_scroll_paused = true;

        self.scroll_offset != before.0 || self.auto_scroll_paused != before.1
    }

    /// Scroll the viewport down by `amount` lines. If this reaches the bottom,
    /// auto-follow resumes. Returns `true` if the position changed.
    pub fn scroll_down(&mut self, amount: usize, total_lines: usize, viewport_height: usize) -> bool {
        if amount == 0 {
            return false;
        }
        if !self.auto_scroll_paused {
            // Already following the tail: no-op.
            return false;
        }

        let max_scroll = total_lines.saturating_sub(viewport_height);
        let before = self.scroll_offset;
        self.scroll_offset = self.scroll_offset.saturating_add(amount);

        if self.scroll_offset >= max_scroll {
            self.follow_bottom();
            true
        } else {
            self.scroll_offset != before
        }
    }

    /// Pause auto-follow and anchor to the current scroll position.
    pub fn pause_auto_scroll(&mut self, _total_lines: usize) {
        if self.auto_scroll_paused {
            return;
        }
        // `scroll_offset` is already absolute (set to `max_scroll` by
        // `resolve_scroll` while following); just flip the flag.
        self.auto_scroll_paused = true;
    }

    /// Resume auto-follow: snap back to the bottom.
    pub fn follow_bottom(&mut self) {
        self.scroll_offset = 0;
        self.auto_scroll_paused = false;
    }

    /// Snap to bottom on any key input (browser-style: typing jumps to cursor).
    pub fn snap_to_bottom_on_input(&mut self) {
        self.scroll_offset = 0;
        self.auto_scroll_paused = false;
    }

    // -- scroll resolution (ported from jcode ui_viewport.rs) -------------

    /// Resolve the final scroll offset for a frame, given the content height
    /// and viewport height. When auto-follow is active the viewport pins to
    /// the bottom (`max_scroll`); when paused it clamps the user's offset.
    ///
    /// Stores `offscreen_above`, `offscreen_below`, and `max_scroll` for the
    /// indicator renderer.
    pub fn resolve_scroll(&mut self, total_lines: usize, viewport_height: usize) -> usize {
        let viewport_height = viewport_height.max(1);
        let max_scroll = total_lines.saturating_sub(viewport_height);

        let scroll = if self.auto_scroll_paused {
            self.scroll_offset.min(max_scroll)
        } else {
            max_scroll
        };

        self.scroll_offset = scroll;
        self.max_scroll = max_scroll;
        self.offscreen_above = scroll;
        self.offscreen_below = max_scroll.saturating_sub(scroll);
        scroll
    }

    // -- indicator rendering ---------------------------------------------

    /// Render scroll indicators onto the cell surface.
    ///
    /// - Up-arrow `{n}` at the top-right when there are offscreen lines above.
    /// - Down-arrow `{n}` at the bottom-right when auto-follow is paused and
    ///   there are offscreen lines below.
    ///
    /// Both use the muted color, matching jcode's `dim_color()` / `queued_color()`.
    pub fn render_indicators(&self, surface: &mut CellSurface, area: CellRect) {
        let muted = theme::MUTED;
        let accent = theme::ACCENT;
        let bg = theme::SURFACE;

        // Up indicator (top-right)
        if self.offscreen_above > 0 {
            let indicator = format!("\u{2191}{}", self.offscreen_above);
            let indicator_len: i32 = indicator.chars().map(|c| char_cell_width(c)).sum();
            let col = area.col + area.cols - indicator_len - 1;
            if col >= 0 {
                surface.put_text(col, area.row, area.cols, muted, bg, &indicator);
            }
        }

        // Down indicator (bottom-right) -- only when auto-follow is paused
        if self.auto_scroll_paused && self.offscreen_below > 0 {
            let indicator = format!("\u{2193}{}", self.offscreen_below);
            let indicator_len: i32 = indicator.chars().map(|c| char_cell_width(c)).sum();
            let col = area.col + area.cols - indicator_len - 1;
            let row = area.row + area.rows - 1;
            if col >= 0 && row < area.row + area.rows {
                surface.put_text(col, row, area.cols, accent, bg, &indicator);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// PromptPreview
// ---------------------------------------------------------------------------

/// Renders a preview band of the last offscreen user prompt when the viewport
/// is scrolled up, mirroring jcode's `prompt_preview` feature.
///
/// The band occupies 1-2 rows at the top of the terminal pane (below any
/// scroll indicator). It shows the prompt number and a truncated preview of
/// the prompt text.
#[derive(Clone, Debug, Default)]
pub struct PromptPreview {
    /// Prompt number (1-indexed) of the last offscreen user prompt.
    pub prompt_number: usize,
    /// Truncated preview text (single line, no newlines).
    pub preview_text: String,
    /// Whether the preview text was truncated (width-overflow).
    pub truncated: bool,
}

impl PromptPreview {
    /// Compute the prompt preview for the current scroll position.
    ///
    /// `user_prompt_starts` are absolute line indices where each user prompt
    /// begins, in order. `user_prompt_texts` are the corresponding text bodies.
    /// `scroll` is the current scroll offset. `area_width` is the available
    /// render width in cells.
    pub fn compute(
        user_prompt_starts: &[usize],
        user_prompt_texts: &[String],
        scroll: usize,
        area_width: i32,
    ) -> Option<Self> {
        if scroll == 0 || user_prompt_starts.is_empty() {
            return None;
        }

        // Find the last user prompt that starts before the scroll offset
        // (i.e. is offscreen above the viewport).
        let prompt_idx = user_prompt_starts
            .partition_point(|&start| start < scroll)
            .checked_sub(1)?;

        let prompt_text = user_prompt_texts.get(prompt_idx)?;
        let trimmed = prompt_text.trim();
        if trimmed.is_empty() {
            return None;
        }

        let prompt_number = prompt_idx + 1;
        let prefix = format!("{}\u{203a} ", prompt_number);
        let prefix_width: i32 = prefix.chars().map(|c| char_cell_width(c)).sum();
        let content_width = (area_width - prefix_width - 2).max(1) as usize;

        let flat = trimmed.replace('\n', " ");
        let char_count = flat.chars().count();
        let (preview_str, truncated) = if char_count > content_width {
            let taken: String = flat.chars().take(content_width.saturating_sub(1)).collect();
            (format!("{}\u{2026}", taken), true)
        } else {
            (flat, false)
        };

        Some(Self {
            prompt_number,
            preview_text: preview_str,
            truncated,
        })
    }

    /// Number of lines the preview band occupies (1 if it fits, 2 if truncated
    /// and the area is tall enough to split). Mirrors jcode's
    /// `compute_prompt_preview_line_count`.
    pub fn line_count(&self, area_width: i32) -> u16 {
        if self.preview_text.is_empty() {
            return 0;
        }
        let prefix = format!("{}\u{203a} ", self.prompt_number);
        let prefix_width: i32 = prefix.chars().map(|c| char_cell_width(c)).sum();
        let content_width = (area_width - prefix_width - 2).max(1) as usize;
        let display_width: i32 = self.preview_text.chars().map(|c| char_cell_width(c)).sum();
        if display_width as usize > content_width {
            2
        } else {
            1
        }
    }

    /// Render the prompt preview band onto the cell surface at the top of the
    /// given area. Returns the number of rows consumed.
    pub fn render(&self, surface: &mut CellSurface, area: CellRect) -> u16 {
        if self.preview_text.is_empty() {
            return 0;
        }

        let muted = theme::MUTED;
        let accent = theme::ACCENT;
        let bg = theme::SURFACE;
        let text_fg = theme::TEXT;

        let prefix = format!("{}\u{203a} ", self.prompt_number);
        let prefix_width: i32 = prefix.chars().map(|c| char_cell_width(c)).sum();
        let content_width = (area.cols - prefix_width - 1).max(1);

        // Truncate the preview text to the content width.
        let truncated = truncate_to_cells(&self.preview_text, content_width as usize);
        let line = format!("{}{}", prefix, truncated);

        surface.put_text(area.col, area.row, area.cols, muted, bg, &line);
        // Put the prefix in accent and the text body in text_fg on top.
        surface.put_text(area.col, area.row, prefix_width, accent, bg, &prefix);
        surface.put_text(
            area.col + prefix_width,
            area.row,
            content_width,
            text_fg,
            bg,
            &truncated,
        );

        1
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_follow_snaps_to_bottom() {
        let mut vp = ViewportState::new();
        let scroll = vp.resolve_scroll(100, 30);
        assert_eq!(scroll, 70);
        assert!(!vp.auto_scroll_paused());
        assert_eq!(vp.offscreen_above, 70);
        assert_eq!(vp.offscreen_below, 0);
    }

    #[test]
    fn scroll_up_pauses_auto_follow() {
        let mut vp = ViewportState::new();
        vp.resolve_scroll(100, 30); // max_scroll = 70
        let changed = vp.scroll_up(10, 100, 30);
        assert!(changed);
        assert!(vp.auto_scroll_paused());
        assert_eq!(vp.scroll_offset, 60); // 70 - 10
    }

    #[test]
    fn scroll_down_to_bottom_resumes_auto_follow() {
        let mut vp = ViewportState::new();
        vp.resolve_scroll(100, 30); // max_scroll = 70
        vp.scroll_up(10, 100, 30); // offset = 60, paused
        let changed = vp.scroll_down(20, 100, 30); // 60 + 20 = 80 >= 70
        assert!(changed);
        assert!(!vp.auto_scroll_paused());
        assert_eq!(vp.scroll_offset, 0);
    }

    #[test]
    fn snap_to_bottom_on_input() {
        let mut vp = ViewportState::new();
        vp.resolve_scroll(100, 30);
        vp.scroll_up(10, 100, 30);
        assert!(vp.auto_scroll_paused());
        vp.snap_to_bottom_on_input();
        assert!(!vp.auto_scroll_paused());
        assert_eq!(vp.scroll_offset, 0);
    }

    #[test]
    fn prompt_preview_compute_finds_last_offscreen() {
        let starts = vec![0, 20, 50];
        let texts = vec![
            "first prompt".to_string(),
            "second prompt".to_string(),
            "third prompt".to_string(),
        ];
        let preview = PromptPreview::compute(&starts, &texts, 25, 80).unwrap();
        assert_eq!(preview.prompt_number, 2);
        assert!(preview.preview_text.contains("second prompt"));
    }

    #[test]
    fn prompt_preview_none_when_at_bottom() {
        let starts = vec![0, 20, 50];
        let texts = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        assert!(PromptPreview::compute(&starts, &texts, 0, 80).is_none());
    }

    #[test]
    fn prompt_preview_truncates_long_text() {
        let starts = vec![0];
        let long_text = "x".repeat(200);
        let texts = vec![long_text];
        let preview = PromptPreview::compute(&starts, &texts, 1, 40).unwrap();
        assert!(preview.truncated);
        assert!(preview.preview_text.ends_with('\u{2026}'));
    }
}
