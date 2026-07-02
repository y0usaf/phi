//! Shared dialog chrome for the built-in overlays, drawn through the
//! extension `DrawContext` (port of the mux's `overlays/dialog.rs`).

use phi_ext::{DrawContext, OverlayPalette};
use phi_tui::display_cell_width;

pub(super) fn render_dialog_title_line(
    ctx: &mut dyn DrawContext,
    row: i32,
    col: i32,
    width: i32,
    title: &str,
    info: &str,
    palette: &OverlayPalette,
) {
    if width <= 0 {
        return;
    }

    ctx.put_text_bold(col, row, width, title, palette.accent, palette.bg);
    let title_width = display_cell_width(title) as i32;
    let info_width = display_cell_width(info) as i32;
    let rule_start = col + title_width + 1;
    let rule_end = col + width - info_width.saturating_add(1);
    // A quiet rule carries the eye from title to info without decoration
    // competing with the dialog body.
    for index in 0..(rule_end - rule_start).max(0) {
        ctx.set_cell(
            rule_start + index,
            row,
            "─",
            palette.border,
            palette.bg,
            false,
            false,
        );
    }
    if info_width > 0 && width > info_width {
        ctx.put_text(
            col + width - info_width,
            row,
            info_width,
            info,
            palette.accent_2,
            palette.bg,
        );
    }
}
