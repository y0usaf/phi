//! Drawing surface trait for extension-provided UI chrome.
//!
//! `DrawContext` is a scoped view over the terminal grid that extensions
//! use to render overlays, sidebar widgets, and statusbar segments. The
//! core owns the underlying `CellSurface`; extensions receive a
//! `&mut dyn DrawContext` bounded to their region.
//!
//! This trait lives in `phi-ext` (not `phi-grid`) so the extension system
//! has no dependency on the grid crate. The mux implements it as a thin
//! adapter over `CellSurface`.

use crate::visual::Rgb;

/// Terminal dimensions exposed to drawing code.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrawSize {
    pub cols: i32,
    pub rows: i32,
}

/// A minimal key event for overlay key handling.
///
/// This mirrors crossterm's `KeyEvent` but lives in `phi-ext` so the
/// extension system doesn't depend on crossterm. The mux converts
/// `crossterm::event::KeyEvent` → `OverlayKey` at the boundary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OverlayKey {
    pub code: OverlayKeyCode,
    pub modifiers: OverlayKeyModifiers,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OverlayKeyModifiers {
    pub control: bool,
    pub shift: bool,
    pub alt: bool,
}

/// Minimal key code enumeration covering the keys overlays need.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayKeyCode {
    Char(char),
    Enter,
    Esc,
    Backspace,
    Tab,
    BackTab,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Delete,
    F(u8),
    Other,
}

impl Default for OverlayKeyCode {
    fn default() -> Self {
        Self::Other
    }
}

/// A rectangular region on the drawing surface.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrawRect {
    pub col: i32,
    pub row: i32,
    pub cols: i32,
    pub rows: i32,
}

impl DrawRect {
    pub fn new(col: i32, row: i32, cols: i32, rows: i32) -> Self {
        Self {
            col,
            row,
            cols,
            rows,
        }
    }

    pub fn inset(self, dx: i32, dy: i32) -> Self {
        Self {
            col: self.col + dx,
            row: self.row + dy,
            cols: (self.cols - 2 * dx).max(0),
            rows: (self.rows - 2 * dy).max(0),
        }
    }
}

/// The drawing surface exposed to extensions.
///
/// All coordinates are 0-based relative to the context's origin. The
/// implementation (in the mux) translates to absolute `CellSurface`
/// coordinates. Colours are `Rgb` from `phi-ext::visual` — the
/// implementation converts to its internal `Color` type.
pub trait DrawContext {
    /// Current surface dimensions.
    fn size(&self) -> DrawSize;

    /// Write text at `(col, row)` with a maximum of `max_cols` cells.
    /// Text is left-aligned and clipped to `max_cols`.
    fn put_text(&mut self, col: i32, row: i32, max_cols: i32, text: &str, fg: Rgb, bg: Rgb);

    /// Write bold text at `(col, row)`.
    fn put_text_bold(&mut self, col: i32, row: i32, max_cols: i32, text: &str, fg: Rgb, bg: Rgb);

    /// Fill a rectangular region with a blank cell (space) using the
    /// given colours.
    fn fill_rect(&mut self, rect: DrawRect, fg: Rgb, bg: Rgb);

    /// Draw a box border around `rect` using `border` colour on `bg`.
    fn draw_box(&mut self, rect: DrawRect, fg: Rgb, bg: Rgb, border: Rgb);

    /// Set a single cell at `(col, row)` with the given text and colours.
    /// `underline` and `reverse` are style flags.
    fn set_cell(
        &mut self,
        col: i32,
        row: i32,
        text: &str,
        fg: Rgb,
        bg: Rgb,
        underline: bool,
        reverse: bool,
    );

    /// Set a cell span (multi-width character) at `(col, row)` spanning
    /// `span` cells.
    fn put_cell_span(
        &mut self,
        col: i32,
        row: i32,
        span: i32,
        text: &str,
        fg: Rgb,
        bg: Rgb,
        underline: bool,
        reverse: bool,
        bold: bool,
    );

    /// Render a scrollbar at the given column.
    fn render_scrollbar(
        &mut self,
        col: i32,
        row: i32,
        rows: i32,
        visible_items: usize,
        total_items: usize,
        scroll_from_top: usize,
        fg: Rgb,
        bg: Rgb,
        track_glyph: &str,
        thumb_fg: Rgb,
        thumb_glyph: &str,
    );

    /// Clear the entire surface to `bg`.
    fn clear(&mut self, bg: Rgb);

    /// Mark the entire surface dirty (forces full repaint).
    fn invalidate(&mut self);
}
