//! Shared dialog chrome (port of pi-harness `overlays/dialog.rs`).

use crate::ui::cell_surface::{CellSurface, display_cell_width};
use crate::ui::theme;

pub fn render_dialog_title_line(
    surface: &mut CellSurface,
    row: i32,
    col: i32,
    width: i32,
    title: &str,
    info: &str,
) {
    if width <= 0 {
        return;
    }

    surface.put_text_bold(col, row, width, theme::ACCENT, theme::SURFACE, title);
    let title_width = display_cell_width(title) as i32;
    let info_width = display_cell_width(info) as i32;
    let rule_start = col + title_width + 1;
    let rule_end = col + width - info_width.saturating_add(1);
    // A quiet rule carries the eye from title to info without decoration
    // competing with the dialog body.
    for index in 0..(rule_end - rule_start).max(0) {
        surface.set_cell(
            rule_start + index,
            row,
            theme::BORDER,
            theme::SURFACE,
            "─",
            false,
        );
    }
    if info_width > 0 && width > info_width {
        surface.put_text(
            col + width - info_width,
            row,
            info_width,
            theme::ACCENT_2,
            theme::SURFACE,
            info,
        );
    }
}
