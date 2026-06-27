//! Terminal pane rendering: screen blit, scrollback scrollbar, hardware cursor.

use super::palette::ScenePalette;
use super::scene::HardwareCursorPos;
use super::terminal_view::for_each_terminal_screen_cell;
use crate::grid::cell_surface::{CellSurface, render_cell_scrollbar};
use crate::grid::layout::CellRect;
use crate::grid::selection::SelectionRange;

const SCROLLBAR_TRACK_GLYPH: &str = "│";
const SCROLLBAR_THUMB_GLYPH: &str = "┃";

pub(super) fn render_terminal(
    surface: &mut CellSurface,
    rect: CellRect,
    screen: Option<&vt100::Screen>,
    selection: Option<SelectionRange>,
    scrollback_total: usize,
    palette: &ScenePalette,
) -> Option<HardwareCursorPos> {
    if rect.rows <= 0 || rect.cols <= 0 {
        return None;
    }
    // The cell loop below writes every pane cell's final value (empty screen
    // cells are emitted as " "), so we only pre-clear when there's no screen
    // to read or its size doesn't match the pane — both are rare (e.g. a
    // resize transition). Skipping the clear on the matched path is what lets
    // `write_cell`'s mark-on-change produce accurate per-cell dirty marks for
    // the terminal pane, so only genuinely-changed cells are repainted.
    let screen = match screen {
        Some(s) => {
            let (screen_rows, screen_cols) = s.size();
            if screen_rows as i32 != rect.rows || screen_cols as i32 != rect.cols {
                surface.fill_rect(rect, palette.term_fg, palette.term_bg);
            }
            s
        }
        None => {
            surface.fill_rect(rect, palette.term_fg, palette.term_bg);
            return None;
        }
    };

    for_each_terminal_screen_cell(
        screen,
        rect.rows.max(0) as u16,
        rect.cols.max(0) as u16,
        selection,
        palette.term_fg,
        palette.term_bg,
        &palette.ansi,
        false,
        |cell| {
            surface.put_cell_span_styled(
                rect.col + i32::from(cell.col),
                rect.row + i32::from(cell.row),
                i32::from(cell.span),
                cell.text,
                cell.fg,
                cell.bg,
                cell.underline,
                false,
                cell.bold,
            );
        },
    );
    render_terminal_scrollback(surface, rect, screen, scrollback_total, palette);

    hardware_cursor_for_screen(rect, screen)
}

fn render_terminal_scrollback(
    surface: &mut CellSurface,
    rect: CellRect,
    screen: &vt100::Screen,
    scrollback_total: usize,
    palette: &ScenePalette,
) {
    let visible_rows = usize::from(screen.size().0);
    let offset = screen.scrollback();
    if visible_rows == 0 || offset == 0 || rect.rows <= 0 {
        return;
    }

    let total_above = scrollback_total.max(offset);
    render_cell_scrollbar(
        surface,
        rect.col + rect.cols - 1,
        rect.row,
        rect.rows,
        visible_rows,
        visible_rows.saturating_add(total_above),
        total_above.saturating_sub(offset),
        palette.border,
        palette.term_bg,
        SCROLLBAR_TRACK_GLYPH,
        palette.accent_2,
        SCROLLBAR_THUMB_GLYPH,
    );
}

fn hardware_cursor_for_screen(rect: CellRect, screen: &vt100::Screen) -> Option<HardwareCursorPos> {
    if screen.scrollback() != 0 || screen.hide_cursor() || rect.cols <= 0 || rect.rows <= 0 {
        return None;
    }

    let (cursor_row, cursor_col) = screen.cursor_position();
    let col = rect.col + i32::from(cursor_col).min(rect.cols.saturating_sub(1));
    let row = rect.row + i32::from(cursor_row).min(rect.rows.saturating_sub(1));
    Some(HardwareCursorPos { col, row })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::theme;

    #[test]
    fn terminal_blit_writes_screen_text_and_cursor() {
        let mut parser = vt100::Parser::new(4, 10, 0);
        parser.process(b"hi");
        let mut surface = CellSurface::new(12, 6, theme::TEXT, theme::TRANSPARENT);
        let rect = CellRect::new(1, 1, 10, 4);
        let cursor = render_terminal(
            &mut surface,
            rect,
            Some(parser.screen()),
            None,
            0,
            &ScenePalette::default(),
        )
        .unwrap();
        assert_eq!(surface.resolve_text(surface.cell(1, 1).unwrap().text), "h");
        assert_eq!(surface.resolve_text(surface.cell(2, 1).unwrap().text), "i");
        assert_eq!((cursor.col, cursor.row), (3, 1));
    }
}
