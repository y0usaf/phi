//! Display-cell width helpers shared by the inline TUI and the mux
//! compositor: terminal-column accounting for monospaced grids.

pub fn display_cell_width(value: &str) -> usize {
    value
        .chars()
        .map(|ch| {
            unicode_width::UnicodeWidthChar::width(ch)
                .unwrap_or(1)
                .max(1)
        })
        .sum()
}

pub fn truncate_to_cells(value: &str, max_cols: usize) -> String {
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
