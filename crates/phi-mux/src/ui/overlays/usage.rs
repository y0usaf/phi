//! Usage table overlay (port of pi-harness `overlays/usage.rs`) over phi's
//! session-JSONL usage scanner.

use phi_tui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use phi_tui::truncate_to_cells;

use super::dialog::render_dialog_title_line;
use phi_mux_grid::cell_surface::{CellSurface, draw_box, render_cell_scrollbar};
use phi_mux_grid::layout::CellRect as Rect;
use crate::ui::scene::ScenePalette;
use phi_mux_grid::theme;
use phi_session::usage::{
    self, UsageReport, is_usage_separator_line, is_usage_total_line, usage_header_line,
    usage_overlay_line_count, usage_overlay_lines, usage_separator_line, usage_total_line,
};

const USAGE_FOOTER_HINT: &str = "\u{2191}/\u{2193}/j/k \u{b7} r reload \u{b7} q/Esc";

#[derive(Clone, Debug)]
pub struct UsageOverlayState {
    pub report: UsageReport,
    pub scroll: usize,
}

impl UsageOverlayState {
    pub fn load() -> Self {
        Self {
            report: usage::load_usage_report(),
            scroll: 0,
        }
    }

    pub fn reload(&mut self) {
        self.report = usage::load_usage_report();
        self.scroll = 0;
    }

    /// Returns `true` when the overlay should close.
    pub fn handle_key(&mut self, key: &KeyEvent, cols: u16, rows: u16) -> bool {
        let mods = key.modifiers;
        if mods.contains(KeyModifiers::CONTROL) {
            return matches!(key.code, KeyCode::Char('c'));
        }
        if mods.contains(KeyModifiers::SHIFT) {
            if matches!(key.code, KeyCode::Char('G')) {
                self.set_scroll(usize::MAX, cols, rows);
            }
            return false;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return true,
            KeyCode::Up | KeyCode::Char('k') => self.scroll_by(-1, cols, rows),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_by(1, cols, rows),
            KeyCode::PageUp => self.page(-1, cols, rows),
            KeyCode::PageDown => self.page(1, cols, rows),
            KeyCode::Home | KeyCode::Char('g') => self.set_scroll(0, cols, rows),
            KeyCode::End => self.set_scroll(usize::MAX, cols, rows),
            KeyCode::Char('r') => self.reload(),
            _ => {}
        }
        false
    }

    pub fn scroll_by(&mut self, delta: i32, cols: u16, rows: u16) {
        let next = if delta < 0 {
            self.scroll.saturating_sub(delta.unsigned_abs() as usize)
        } else {
            self.scroll.saturating_add(delta as usize)
        };
        self.set_scroll(next, cols, rows);
    }

    fn page(&mut self, delta_pages: i32, cols: u16, rows: u16) {
        let visible = visible_rows(cols, rows).max(1);
        self.scroll_by(delta_pages.saturating_mul(visible as i32), cols, rows);
    }

    fn set_scroll(&mut self, scroll: usize, cols: u16, rows: u16) {
        let visible = visible_rows(cols, rows).max(1);
        self.scroll = scroll.min(usage_overlay_line_count(&self.report).saturating_sub(visible));
    }

    fn clamp_scroll(&mut self, visible_rows: usize) {
        let lines = usage_overlay_line_count(&self.report);
        self.scroll = self.scroll.min(lines.saturating_sub(visible_rows));
    }
}

fn usage_overlay_rect(cols: i32, rows: i32) -> Rect {
    let cols = cols.max(1);
    let rows = rows.max(1);
    let width = cols.clamp(1, 118);
    let height = rows.clamp(1, 34);
    Rect::new((cols - width) / 2, (rows - height) / 2, width, height)
}

fn visible_rows(cols: u16, rows: u16) -> usize {
    usage_overlay_rect(i32::from(cols), i32::from(rows))
        .rows
        .saturating_sub(8) as usize
}

pub fn render_usage_overlay(
    surface: &mut CellSurface,
    usage: &mut UsageOverlayState,
    palette: &ScenePalette,
) {
    let rect = usage_overlay_rect(surface.cols, surface.rows);
    draw_box(
        surface,
        rect,
        palette.fg,
        palette.surface_raised,
        palette.statusbar_bg,
    );
    if rect.cols <= 2 || rect.rows <= 2 {
        return;
    }

    let inner = rect.inset_edges(2, 1, 2, 1);
    surface.fill_rect(inner, palette.fg, palette.bg);

    let count = format!(" {} ", usage.report.entries);
    render_dialog_title_line(surface, inner.row, inner.col, inner.cols, " USAGE ", &count, palette);

    let header_row = inner.row + 1;
    let header_separator_row = inner.row + 2;
    let total_row = inner.row + 3;
    let total_separator_row = inner.row + 4;
    let list_row = inner.row + 5;
    let footer_row = inner.row + inner.rows - 1;
    let list_rows = (footer_row - list_row).max(0) as usize;
    let list_width = (inner.cols - 1).max(0);

    let header_line = usage_header_line();
    surface.put_text(
        inner.col,
        header_row,
        inner.cols,
        theme::brighten(palette.muted, 24),
        palette.bg,
        &header_line,
    );

    let separator_line = usage_separator_line();
    surface.put_text(
        inner.col,
        header_separator_row,
        inner.cols,
        palette.border,
        palette.bg,
        &separator_line,
    );

    let total_line = usage_total_line(&usage.report.totals);
    surface.put_text_bold(
        inner.col,
        total_row,
        inner.cols,
        palette.fg,
        palette.bg,
        &total_line,
    );
    surface.put_text(
        inner.col,
        total_separator_row,
        inner.cols,
        palette.border,
        palette.bg,
        &separator_line,
    );

    usage.clamp_scroll(list_rows);
    let lines = usage_overlay_lines(&usage.report);

    for (row_offset, line) in lines.iter().skip(usage.scroll).take(list_rows).enumerate() {
        let row = list_row + row_offset as i32;
        let is_separator = is_usage_separator_line(line);
        let is_total = is_usage_total_line(line);
        let fg = if is_separator {
            palette.border
        } else if is_total {
            palette.fg
        } else {
            theme::brighten(palette.muted, 32)
        };
        let text = truncate_to_cells(line, list_width as usize);
        if is_total {
            surface.put_text_bold(inner.col, row, list_width, fg, palette.bg, &text);
        } else {
            surface.put_text(inner.col, row, list_width, fg, palette.bg, &text);
        }
    }
    render_cell_scrollbar(
        surface,
        inner.col + inner.cols - 1,
        list_row,
        list_rows as i32,
        list_rows,
        usage_overlay_line_count(&usage.report),
        usage.scroll,
        palette.border,
        palette.bg,
        "╎",
        palette.accent_2,
        "┃",
    );

    surface.put_text(
        inner.col,
        footer_row,
        inner.cols,
        palette.muted,
        palette.bg,
        &truncate_to_cells(USAGE_FOOTER_HINT, inner.cols.max(0) as usize),
    );
}
