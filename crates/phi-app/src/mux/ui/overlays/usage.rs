//! Usage table overlay (port of pi-harness `overlays/usage.rs`) over phi's
//! session-JSONL usage scanner.

use phi_tui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::dialog::render_dialog_title_line;
use crate::mux::ui::cell_surface::{
    CellSurface, display_cell_width, draw_box, render_cell_scrollbar, truncate_to_cells,
};
use crate::mux::ui::layout::CellRect as Rect;
use crate::mux::ui::theme;
use crate::mux::usage::{self, UsageDay, UsageModelBreakdown, UsageReport, UsageTotals};

const USAGE_TREE_COLS: usize = 40;
const USAGE_VALUE_COLS: [usize; 6] = [10, 10, 10, 10, 10, 9];
const USAGE_FOOTER_HINT: &str = "↑/↓/j/k · r reload · q/Esc";

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

pub fn usage_overlay_line_count(report: &UsageReport) -> usize {
    let data_rows = report
        .days
        .iter()
        .map(|day| 1 + day.model_breakdowns.len())
        .sum::<usize>();
    data_rows.saturating_add(report.days.len().saturating_sub(1))
}

/// Scrollable day/model rows; the total row stays fixed in the overlay.
pub fn usage_overlay_lines(report: &UsageReport) -> Vec<String> {
    let mut lines = Vec::with_capacity(usage_overlay_line_count(report));
    for (day_index, day) in report.days.iter().enumerate() {
        lines.push(usage_day_line(day));
        let models = sorted_model_breakdowns(day);
        for (index, breakdown) in models.iter().enumerate() {
            lines.push(usage_model_line(breakdown, index + 1 == models.len()));
        }
        if day_index + 1 < report.days.len() {
            lines.push(usage_separator_line());
        }
    }
    lines
}

pub fn render_usage_overlay(surface: &mut CellSurface, usage: &mut UsageOverlayState) {
    let rect = usage_overlay_rect(surface.cols, surface.rows);
    draw_box(
        surface,
        rect,
        theme::TEXT,
        theme::SURFACE_RAISED,
        theme::STATUS_BG,
    );
    if rect.cols <= 2 || rect.rows <= 2 {
        return;
    }

    let inner = rect.inset_edges(2, 1, 2, 1);
    surface.fill_rect(inner, theme::TEXT, theme::SURFACE);

    let count = format!(" {} ", usage.report.entries);
    render_dialog_title_line(surface, inner.row, inner.col, inner.cols, " USAGE ", &count);

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
        theme::brighten(theme::MUTED, 24),
        theme::SURFACE,
        &header_line,
    );

    let separator_line = usage_separator_line();
    surface.put_text(
        inner.col,
        header_separator_row,
        inner.cols,
        theme::BORDER,
        theme::SURFACE,
        &separator_line,
    );

    let total_line = usage_total_line(&usage.report.totals);
    surface.put_text_bold(
        inner.col,
        total_row,
        inner.cols,
        theme::TEXT,
        theme::SURFACE,
        &total_line,
    );
    surface.put_text(
        inner.col,
        total_separator_row,
        inner.cols,
        theme::BORDER,
        theme::SURFACE,
        &separator_line,
    );

    usage.clamp_scroll(list_rows);
    let lines = usage_overlay_lines(&usage.report);

    for (row_offset, line) in lines.iter().skip(usage.scroll).take(list_rows).enumerate() {
        let row = list_row + row_offset as i32;
        let is_separator = is_usage_separator_line(line);
        let is_total = is_usage_total_line(line);
        let fg = if is_separator {
            theme::BORDER
        } else if is_total {
            theme::TEXT
        } else {
            theme::brighten(theme::MUTED, 32)
        };
        let text = truncate_to_cells(line, list_width as usize);
        if is_total {
            surface.put_text_bold(inner.col, row, list_width, fg, theme::SURFACE, &text);
        } else {
            surface.put_text(inner.col, row, list_width, fg, theme::SURFACE, &text);
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
        theme::BORDER,
        theme::SURFACE,
        "╎",
        theme::ACCENT_2,
        "┃",
    );

    surface.put_text(
        inner.col,
        footer_row,
        inner.cols,
        theme::MUTED,
        theme::SURFACE,
        &truncate_to_cells(USAGE_FOOTER_HINT, inner.cols.max(0) as usize),
    );
}

fn usage_header_line() -> String {
    usage_table_line(
        "DATE / MODEL",
        ["INPUT", "OUTPUT", "CACHE+", "CACHE↺", "TOTAL", "COST"],
    )
}

fn usage_separator_line() -> String {
    let mut line = "─".repeat(USAGE_TREE_COLS);
    for width in USAGE_VALUE_COLS {
        line.push('┼');
        line.push_str(&"─".repeat(width));
    }
    line
}

fn is_usage_separator_line(line: &str) -> bool {
    line.starts_with('─')
}

fn is_usage_total_line(line: &str) -> bool {
    !is_usage_separator_line(line) && !line.starts_with("  ├─ ") && !line.starts_with("  └─ ")
}

fn usage_total_line(totals: &UsageTotals) -> String {
    usage_row_line("TOTAL", totals)
}

fn usage_day_line(day: &UsageDay) -> String {
    usage_row_line(&day.date, &day.totals)
}

fn usage_model_line(breakdown: &UsageModelBreakdown, is_last: bool) -> String {
    let branch = if is_last { "  └─ " } else { "  ├─ " };
    let label = format!("{branch}{}", format_model_name(&breakdown.model_name));
    usage_row_line(&label, &breakdown.totals)
}

fn sorted_model_breakdowns(day: &UsageDay) -> Vec<UsageModelBreakdown> {
    let mut models = day.model_breakdowns.clone();
    models.sort_by_key(|a| format_model_name(&a.model_name));
    models
}

fn usage_row_line(label: &str, totals: &UsageTotals) -> String {
    let input = format_u64(totals.input_tokens);
    let output = format_u64(totals.output_tokens);
    let cache_creation = format_u64(totals.cache_creation_tokens);
    let cache_read = format_u64(totals.cache_read_tokens);
    let total = format_u64(totals.total_tokens());
    let cost = format_currency(totals.total_cost);
    usage_table_line(
        label,
        [&input, &output, &cache_creation, &cache_read, &total, &cost],
    )
}

fn usage_table_line(label: &str, values: [&str; 6]) -> String {
    let mut line = format_cell_left(label, USAGE_TREE_COLS);
    for (value, width) in values.iter().zip(USAGE_VALUE_COLS.iter()) {
        line.push('│');
        line.push_str(&format_cell_right(value, *width));
    }
    line
}

fn format_cell_left(value: &str, width: usize) -> String {
    let value = truncate_to_cells(value, width);
    let padding = width.saturating_sub(display_cell_width(&value));
    format!("{value}{}", " ".repeat(padding))
}

fn format_cell_right(value: &str, width: usize) -> String {
    let value = truncate_to_cells(value, width);
    let padding = width.saturating_sub(display_cell_width(&value));
    format!("{}{value}", " ".repeat(padding))
}

fn format_model_name(model: &str) -> String {
    if let Some(rest) = model.strip_prefix("anthropic/") {
        return format_model_name(rest);
    }

    let Some(rest) = model.strip_prefix("claude-") else {
        return model.to_string();
    };

    let mut parts = rest.split('-').collect::<Vec<_>>();
    if parts
        .last()
        .is_some_and(|part| part.len() == 8 && part.chars().all(|ch| ch.is_ascii_digit()))
    {
        parts.pop();
    }
    parts.join("-")
}

fn format_u64(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

fn format_currency(amount: f64) -> String {
    format!("${amount:.2}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_model_names_like_pi_usage() {
        assert_eq!(format_model_name("claude-opus-4-5"), "opus-4-5");
        assert_eq!(format_model_name("claude-sonnet-4-20250514"), "sonnet-4");
        assert_eq!(format_model_name("anthropic/claude-opus-4.5"), "opus-4.5");
    }

    #[test]
    fn usage_rows_include_totals_and_cost() {
        let report = UsageReport {
            days: vec![UsageDay {
                date: "2026-01-02".into(),
                totals: UsageTotals {
                    input_tokens: 10,
                    output_tokens: 12,
                    cache_creation_tokens: 1,
                    cache_read_tokens: 2,
                    total_cost: 0.03,
                },
                model_breakdowns: vec![
                    UsageModelBreakdown {
                        model_name: "claude-sonnet-4".into(),
                        totals: UsageTotals {
                            input_tokens: 7,
                            output_tokens: 8,
                            cache_creation_tokens: 1,
                            cache_read_tokens: 2,
                            total_cost: 0.01,
                        },
                    },
                    UsageModelBreakdown {
                        model_name: "claude-opus-4-5".into(),
                        totals: UsageTotals {
                            input_tokens: 3,
                            output_tokens: 4,
                            cache_creation_tokens: 0,
                            cache_read_tokens: 0,
                            total_cost: 0.02,
                        },
                    },
                ],
            }],
            totals: UsageTotals {
                input_tokens: 1000,
                output_tokens: 50,
                cache_creation_tokens: 20,
                cache_read_tokens: 10,
                total_cost: 0.055,
            },
            files_scanned: 1,
            entries: 1,
            skipped_duplicates: 0,
        };

        let daily_rows = usage_overlay_lines(&report);
        let header_row = usage_header_line();
        let separator_row = usage_separator_line();

        assert_eq!(
            display_cell_width(&header_row),
            display_cell_width(&separator_row)
        );
        assert!(header_row.contains("DATE / MODEL"));
        assert!(header_row.contains('│'));
        assert!(separator_row.contains('┼'));
        assert_eq!(daily_rows.len(), 3);
        assert!(daily_rows[0].contains("2026-01-02"));
        assert!(daily_rows[0].contains('│'));
        assert!(daily_rows[0].contains("25"));
        assert!(daily_rows[0].contains("$0.03"));
        assert!(daily_rows[1].contains("├─ opus-4-5"));
        assert!(daily_rows[1].contains("$0.02"));
        assert!(daily_rows[2].contains("└─ sonnet-4"));
        assert!(daily_rows[2].contains("$0.01"));

        let total_row = usage_total_line(&report.totals);

        assert!(total_row.contains("TOTAL"));
        assert!(total_row.contains("1,080"));
        assert!(total_row.contains("$0.06"));
    }

    #[test]
    fn usage_lines_separate_day_groups() {
        let day = |date: &str| UsageDay {
            date: date.into(),
            totals: UsageTotals {
                input_tokens: 1,
                output_tokens: 2,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
                total_cost: 0.01,
            },
            model_breakdowns: Vec::new(),
        };
        let report = UsageReport {
            days: vec![day("2026-01-02"), day("2026-01-01")],
            totals: UsageTotals::default(),
            files_scanned: 1,
            entries: 2,
            skipped_duplicates: 0,
        };

        let rows = usage_overlay_lines(&report);

        assert_eq!(usage_overlay_line_count(&report), 3);
        assert_eq!(rows.len(), 3);
        assert!(rows[1].starts_with('─'));
        assert!(rows[1].contains('┼'));
    }
}
