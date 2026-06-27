pub(super) fn display_cell_width(value: &str) -> usize {
    value
        .chars()
        .map(|ch| {
            unicode_width::UnicodeWidthChar::width(ch)
                .unwrap_or(1)
                .max(1)
        })
        .sum()
}

fn truncate_to_cells(value: &str, max_cols: usize) -> String {
    if display_cell_width(value) <= max_cols {
        return value.to_string();
    }
    if max_cols == 0 {
        return String::new();
    }
    let ellipsis = "\u{2026}";
    let ellipsis_w = display_cell_width(ellipsis);
    if max_cols <= ellipsis_w {
        return ellipsis.to_string();
    }
    let mut out = String::new();
    let mut width = 0;
    for ch in value.chars() {
        let ch_width = unicode_width::UnicodeWidthChar::width(ch)
            .unwrap_or(1)
            .max(1);
        if width + ch_width + ellipsis_w > max_cols {
            break;
        }
        out.push(ch);
        width += ch_width;
    }
    out.push_str(ellipsis);
    out
}

use super::types::*;

// --- Usage table lines: pure text formatting shared by the inline TUI's
// /usage scrollback dump and the mux's usage overlay, so both modes render
// identical tables. ---

const USAGE_TREE_COLS: usize = 40;
const USAGE_VALUE_COLS: [usize; 6] = [10, 10, 10, 10, 10, 9];

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

pub fn usage_header_line() -> String {
    usage_table_line(
        "DATE / MODEL",
        ["INPUT", "OUTPUT", "CACHE+", "CACHE↺", "TOTAL", "COST"],
    )
}

pub fn usage_separator_line() -> String {
    let mut line = "─".repeat(USAGE_TREE_COLS);
    for width in USAGE_VALUE_COLS {
        line.push('┼');
        line.push_str(&"─".repeat(width));
    }
    line
}

pub fn is_usage_separator_line(line: &str) -> bool {
    line.starts_with('─')
}

pub fn is_usage_total_line(line: &str) -> bool {
    !is_usage_separator_line(line) && !line.starts_with("  ├─ ") && !line.starts_with("  └─ ")
}

pub fn usage_total_line(totals: &UsageTotals) -> String {
    usage_row_line("TOTAL", totals)
}

pub(super) fn usage_day_line(day: &UsageDay) -> String {
    usage_row_line(&day.date, &day.totals)
}

pub(super) fn usage_model_line(breakdown: &UsageModelBreakdown, is_last: bool) -> String {
    let branch = if is_last { "  └─ " } else { "  ├─ " };
    let label = format!("{branch}{}", format_model_name(&breakdown.model_name));
    usage_row_line(&label, &breakdown.totals)
}

pub(super) fn sorted_model_breakdowns(day: &UsageDay) -> Vec<UsageModelBreakdown> {
    let mut models = day.model_breakdowns.clone();
    models.sort_by_key(|a| format_model_name(&a.model_name));
    models
}

pub(super) fn usage_row_line(label: &str, totals: &UsageTotals) -> String {
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

pub(super) fn usage_table_line(label: &str, values: [&str; 6]) -> String {
    let mut line = format_cell_left(label, USAGE_TREE_COLS);
    for (value, width) in values.iter().zip(USAGE_VALUE_COLS.iter()) {
        line.push('│');
        line.push_str(&format_cell_right(value, *width));
    }
    line
}

pub(super) fn format_cell_left(value: &str, width: usize) -> String {
    let value = truncate_to_cells(value, width);
    let padding = width.saturating_sub(display_cell_width(&value));
    format!("{value}{}", " ".repeat(padding))
}

pub(super) fn format_cell_right(value: &str, width: usize) -> String {
    let value = truncate_to_cells(value, width);
    let padding = width.saturating_sub(display_cell_width(&value));
    format!("{}{value}", " ".repeat(padding))
}

pub(crate) fn format_model_name(model: &str) -> String {
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

pub(super) fn format_u64(value: u64) -> String {
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

pub(super) fn format_currency(amount: f64) -> String {
    format!("${amount:.2}")
}
