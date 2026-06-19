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

#[cfg(test)]
mod tests {
    use super::*;

    // Lock `⎿` (U+23BF, "bottom-left crop") to width 1. It's the nesting gutter
    // char used by the flat TUI (`nest_prefix`), and the body-wrap width math
    // assumes `"⎿ "` is exactly 2 cells. U+23BF is in the "ambiguous" East Asian
    // Width category, so `unicode_width` returns `Some(1)` (never 2); this test
    // fails loudly if a future unicode-width upgrade ever reclassifies it.
    #[test]
    fn crop_mark_is_width_one() {
        assert_eq!(display_cell_width("⎿"), 1);
        assert_eq!(display_cell_width("⎿ "), 2);
        assert_eq!(display_cell_width("⎿ ⎿ "), 4);
    }
}
