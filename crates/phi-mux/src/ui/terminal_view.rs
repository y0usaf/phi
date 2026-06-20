//! Visit visible vt100 screen cells with resolved colors (port of
//! pi-harness `app::terminal_view`).

use super::color::Color;
use super::selection::{SelectionRange, selection_span};
use super::theme::screen_cell_colors;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalCellView<'a> {
    pub row: u16,
    pub col: u16,
    pub span: u16,
    pub text: &'a str,
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub underline: bool,
}

#[allow(clippy::too_many_arguments)]
pub fn for_each_terminal_screen_cell<F>(
    screen: &vt100::Screen,
    rows: u16,
    cols: u16,
    selection: Option<SelectionRange>,
    default_fg: Color,
    default_bg: Color,
    ansi_palette: &[Color; 16],
    draw_cursor: bool,
    mut visit: F,
) where
    F: FnMut(TerminalCellView<'_>),
{
    let (screen_rows, screen_cols) = screen.size();
    let (cursor_row, cursor_col) = screen.cursor_position();
    let cursor_visible = draw_cursor && screen.scrollback() == 0 && !screen.hide_cursor();

    for row in 0..screen_rows.min(rows) {
        let row_selection = selection_span(selection, row, screen_cols);
        for col in 0..screen_cols.min(cols) {
            let Some(cell) = screen.cell(row, col) else {
                continue;
            };
            if cell.is_wide_continuation() {
                continue;
            }

            let span = if col + 1 < screen_cols
                && screen
                    .cell(row, col + 1)
                    .is_some_and(|next| next.is_wide_continuation())
            {
                2
            } else {
                1
            };
            let selected = row_selection.is_some_and(|(start, width)| {
                let end = start + width;
                let cell_end = col + span;
                end > col && start < cell_end
            });
            let cursor_here = cursor_visible && row == cursor_row && col == cursor_col;
            let (fg, bg) = screen_cell_colors(
                cell,
                cursor_here,
                selected,
                default_fg,
                default_bg,
                ansi_palette,
            );

            visit(TerminalCellView {
                row,
                col,
                span,
                text: if cell.contents().is_empty() {
                    " "
                } else {
                    cell.contents()
                },
                fg,
                bg,
                bold: cell.bold(),
                underline: cell.underline(),
            });
        }
    }
}
