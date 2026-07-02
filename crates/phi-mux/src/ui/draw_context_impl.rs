//! Adapter implementing `phi_ext::DrawContext` over the mux's `CellSurface`.
//!
//! This is the bridge between the extension system's drawing API and the
//! mux's grid renderer. The core owns the `CellSurface`; extensions receive
//! a `MuxDrawContext` wrapping a `&mut CellSurface` with palette colours.

use crate::grid::cell_surface::{CellSurface, draw_box, render_cell_scrollbar};
use crate::grid::color::Color;
use crate::grid::layout::CellRect;
use crate::ui::palette::ScenePalette;
use phi_ext::{DrawContext, DrawRect, DrawSize, OverlayPalette, Rgb};

/// Convert an `phi_ext::Rgb` to the grid's `Color`.
fn rgb_to_color(rgb: Rgb) -> Color {
    Color::rgb(rgb.0, rgb.1, rgb.2)
}

/// Convert a grid `Color` to an `phi_ext::Rgb`.
fn color_to_rgb(color: Color) -> Rgb {
    let (r, g, b) = color.rgb_components();
    Rgb(r, g, b)
}

/// Build the `OverlayPalette` extensions see from the mux's resolved scene
/// palette. The scene's background is TRANSPARENT (terminal default), which
/// `Rgb` can't express — substitute the terminal's OSC-reported background.
pub fn overlay_palette(palette: &ScenePalette, terminal_bg: phi_tui::Rgb) -> OverlayPalette {
    OverlayPalette {
        fg: color_to_rgb(palette.fg),
        bg: Rgb(terminal_bg.0, terminal_bg.1, terminal_bg.2),
        muted: color_to_rgb(palette.muted),
        border: color_to_rgb(palette.border),
        accent: color_to_rgb(palette.accent),
        accent_2: color_to_rgb(palette.accent_2),
        surface_raised: color_to_rgb(palette.surface_raised),
        statusbar_bg: color_to_rgb(palette.statusbar_bg),
    }
}

/// A scoped drawing context backed by a `CellSurface`.
///
/// All coordinates are absolute (0-based from the surface origin). The
/// palette colours are used as defaults when the caller doesn't specify
/// explicit colours.
pub struct MuxDrawContext<'a> {
    surface: &'a mut CellSurface,
    palette: OverlayPalette,
}

impl<'a> MuxDrawContext<'a> {
    pub fn new(surface: &'a mut CellSurface, palette: OverlayPalette) -> Self {
        Self { surface, palette }
    }
}

impl<'a> DrawContext for MuxDrawContext<'a> {
    fn size(&self) -> DrawSize {
        DrawSize {
            cols: self.surface.cols,
            rows: self.surface.rows,
        }
    }

    fn palette(&self) -> OverlayPalette {
        self.palette
    }

    fn put_text(&mut self, col: i32, row: i32, max_cols: i32, text: &str, fg: Rgb, bg: Rgb) {
        self.surface
            .put_text(col, row, max_cols, rgb_to_color(fg), rgb_to_color(bg), text);
    }

    fn put_text_bold(&mut self, col: i32, row: i32, max_cols: i32, text: &str, fg: Rgb, bg: Rgb) {
        self.surface
            .put_text_bold(col, row, max_cols, rgb_to_color(fg), rgb_to_color(bg), text);
    }

    fn fill_rect(&mut self, rect: DrawRect, fg: Rgb, bg: Rgb) {
        let cell_rect = CellRect::new(rect.col, rect.row, rect.cols, rect.rows);
        self.surface
            .fill_rect(cell_rect, rgb_to_color(fg), rgb_to_color(bg));
    }

    fn draw_box(&mut self, rect: DrawRect, fg: Rgb, bg: Rgb, border: Rgb) {
        let cell_rect = CellRect::new(rect.col, rect.row, rect.cols, rect.rows);
        draw_box(
            self.surface,
            cell_rect,
            rgb_to_color(fg),
            rgb_to_color(bg),
            rgb_to_color(border),
        );
    }

    fn set_cell(
        &mut self,
        col: i32,
        row: i32,
        text: &str,
        fg: Rgb,
        bg: Rgb,
        underline: bool,
        reverse: bool,
    ) {
        self.surface.set_cell_styled(
            col,
            row,
            rgb_to_color(fg),
            rgb_to_color(bg),
            text,
            underline,
            reverse,
        );
    }

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
    ) {
        self.surface.put_cell_span_styled(
            col,
            row,
            span,
            text,
            rgb_to_color(fg),
            rgb_to_color(bg),
            underline,
            reverse,
            bold,
        );
    }

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
    ) {
        render_cell_scrollbar(
            self.surface,
            col,
            row,
            rows,
            visible_items,
            total_items,
            scroll_from_top,
            rgb_to_color(fg),
            rgb_to_color(bg),
            track_glyph,
            rgb_to_color(thumb_fg),
            thumb_glyph,
        );
    }

    fn clear(&mut self, bg: Rgb) {
        let fg = Color::rgb(255, 255, 255);
        self.surface.fill_rect(
            CellRect::new(0, 0, self.surface.cols, self.surface.rows),
            fg,
            rgb_to_color(bg),
        );
    }

    fn invalidate(&mut self) {
        self.surface.invalidate();
    }
}

/// Convert a crossterm `KeyEvent` to an `OverlayKey` for extension overlays.
pub fn key_event_to_overlay_key(key: &phi_tui::crossterm::event::KeyEvent) -> phi_ext::OverlayKey {
    use phi_ext::{OverlayKeyCode, OverlayKeyModifiers};
    use phi_tui::crossterm::event::{KeyCode, KeyModifiers};

    let code = match key.code {
        KeyCode::Char(c) => OverlayKeyCode::Char(c),
        KeyCode::Enter => OverlayKeyCode::Enter,
        KeyCode::Esc => OverlayKeyCode::Esc,
        KeyCode::Backspace => OverlayKeyCode::Backspace,
        KeyCode::Tab => OverlayKeyCode::Tab,
        KeyCode::BackTab => OverlayKeyCode::BackTab,
        KeyCode::Up => OverlayKeyCode::Up,
        KeyCode::Down => OverlayKeyCode::Down,
        KeyCode::Left => OverlayKeyCode::Left,
        KeyCode::Right => OverlayKeyCode::Right,
        KeyCode::Home => OverlayKeyCode::Home,
        KeyCode::End => OverlayKeyCode::End,
        KeyCode::PageUp => OverlayKeyCode::PageUp,
        KeyCode::PageDown => OverlayKeyCode::PageDown,
        KeyCode::Delete => OverlayKeyCode::Delete,
        KeyCode::F(n) => OverlayKeyCode::F(n),
        _ => OverlayKeyCode::Other,
    };

    let modifiers = OverlayKeyModifiers {
        control: key.modifiers.contains(KeyModifiers::CONTROL),
        shift: key.modifiers.contains(KeyModifiers::SHIFT),
        alt: key.modifiers.contains(KeyModifiers::ALT),
    };

    phi_ext::OverlayKey { code, modifiers }
}
