//! Shared dialog chrome (port of pi-harness `overlays/dialog.rs`).

use crate::mux::ui::cell_surface::{CellSurface, display_cell_width};
use crate::mux::ui::color::Color;
use crate::mux::ui::theme;

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

    surface.put_text(col, row, width, theme::TEXT, theme::SURFACE, title);
    let title_width = display_cell_width(title) as i32;
    let info_width = display_cell_width(info) as i32;
    let slash_start = col + title_width + 1;
    let slash_end = col + width - info_width.saturating_add(1);
    let slash_count = (slash_end - slash_start).max(0) as usize;
    for index in 0..slash_count {
        let fg = gradient_color(theme::ACCENT, theme::ACCENT_2, index, slash_count);
        surface.set_cell(
            slash_start + index as i32,
            row,
            fg,
            theme::SURFACE,
            "╱",
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

fn gradient_color(from: Color, to: Color, index: usize, len: usize) -> Color {
    if len <= 1 || from.ansi_index_value().is_some() || to.ansi_index_value().is_some() {
        return from;
    }

    let (r1, g1, b1) = from.rgb_components();
    let (r2, g2, b2) = to.rgb_components();
    let denom = (len - 1) as u32;
    let pos = index as u32;
    let blend = |a: u8, b: u8| -> u8 {
        let a = u32::from(a);
        let b = u32::from(b);
        (((a * (denom - pos)) + (b * pos)) / denom) as u8
    };
    Color::rgb(blend(r1, r2), blend(g1, g2), blend(b1, b2))
}
