use std::sync::LazyLock;

use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::SyntaxSet;

use crate::palette::TerminalColors;

// Basic ANSI controls — these reference the terminal's own palette (indices
// 0/1/3/4/6 etc.), so they already follow the user's terminal theme.
pub(super) const RESET: &str = "\x1b[0m";
pub(super) const BOLD: &str = "\x1b[1m";
pub(super) const DIM: &str = "\x1b[2m";
pub(super) const ITALIC: &str = "\x1b[3m";
pub(super) const UNDERLINE: &str = "\x1b[4m";
pub(super) const CYAN: &str = "\x1b[36m";

pub(super) static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
pub(super) static THEME: LazyLock<Theme> = LazyLock::new(|| {
    ThemeSet::load_defaults()
        .themes
        .remove("base16-ocean.dark")
        .expect("bundled theme")
});

/// Truecolor ANSI escapes for markdown/output highlighting, derived from the
/// host terminal's reported colors when available. Mirrors opencode's
/// `generateSystem` mapping: ANSI accents → palette roles. `default()` returns
/// the previous hardcoded values (base16-ocean family) as the fallback.
#[derive(Clone)]
pub struct MarkdownPalette {
    pub repl_fence: String,
    pub repl_fn: String,
    pub marker_accent: String,
    pub task_done: String,
    pub quote_bar: String,
    pub subhead: String,
    pub out_base: String,
    pub out_err: String,
    pub out_warn: String,
    pub out_ok: String,
    pub out_path: String,
    pub out_num: String,
    pub out_str: String,
    pub out_url: String,
    pub diff_add: String,
    pub diff_del: String,
}

impl MarkdownPalette {
    /// Previous hardcoded values (base16-ocean family).
    pub fn default_const() -> Self {
        Self {
            repl_fence: s("\x1b[38;2;180;140;255m"),
            repl_fn: s("\x1b[38;2;255;210;120m"),
            marker_accent: s("\x1b[38;2;130;160;230m"),
            task_done: s("\x1b[38;2;130;200;130m"),
            quote_bar: s("\x1b[38;2;150;130;220m"),
            subhead: s("\x1b[38;2;190;150;255m"),
            out_base: s("\x1b[38;2;150;155;165m"),
            out_err: s("\x1b[38;2;240;120;120m"),
            out_warn: s("\x1b[38;2;235;185;100m"),
            out_ok: s("\x1b[38;2;130;200;140m"),
            out_path: s("\x1b[38;2;130;180;230m"),
            out_num: s("\x1b[38;2;170;150;230m"),
            out_str: s("\x1b[38;2;150;190;150m"),
            out_url: s("\x1b[38;2;120;200;220m"),
            diff_add: s("\x1b[38;2;120;200;130m"),
            diff_del: s("\x1b[38;2;230;110;110m"),
        }
    }

    /// Derive palette from terminal-reported colors (OSC 10/11/4), mirroring
    /// opencode's `generateSystem`: ANSI palette → markdown roles.
    pub fn from_terminal_colors(colors: &TerminalColors) -> Self {
        let col = |i: usize| {
            colors.palette[i].map(|c| format!("\x1b[38;2;{};{};{}m", c.0, c.1, c.2))
        };
        let fallback = Self::default_const();
        Self {
            repl_fence: col(5).unwrap_or_else(|| fallback.repl_fence.clone()),
            repl_fn: col(3).unwrap_or_else(|| fallback.repl_fn.clone()),
            marker_accent: col(4).unwrap_or_else(|| fallback.marker_accent.clone()),
            task_done: col(2).unwrap_or_else(|| fallback.task_done.clone()),
            quote_bar: col(5).unwrap_or_else(|| fallback.quote_bar.clone()),
            subhead: col(13).unwrap_or_else(|| fallback.subhead.clone()),
            out_base: col(7).unwrap_or_else(|| fallback.out_base.clone()),
            out_err: col(1).unwrap_or_else(|| fallback.out_err.clone()),
            out_warn: col(3).unwrap_or_else(|| fallback.out_warn.clone()),
            out_ok: col(2).unwrap_or_else(|| fallback.out_ok.clone()),
            out_path: col(4).unwrap_or_else(|| fallback.out_path.clone()),
            out_num: col(3).unwrap_or_else(|| fallback.out_num.clone()),
            out_str: col(2).unwrap_or_else(|| fallback.out_str.clone()),
            out_url: col(6).unwrap_or_else(|| fallback.out_url.clone()),
            diff_add: col(2).unwrap_or_else(|| fallback.diff_add.clone()),
            diff_del: col(1).unwrap_or_else(|| fallback.diff_del.clone()),
        }
    }
}

impl Default for MarkdownPalette {
    fn default() -> Self {
        Self::default_const()
    }
}

fn s(v: &str) -> String {
    v.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette::Rgb;

    #[test]
    fn default_const_matches_previous_hardcoded_values() {
        let p = MarkdownPalette::default_const();
        assert_eq!(p.repl_fence, "\x1b[38;2;180;140;255m");
        assert_eq!(p.diff_del, "\x1b[38;2;230;110;110m");
    }

    #[test]
    fn from_terminal_colors_maps_ansi_roles() {
        let colors = TerminalColors {
            background: Rgb(20, 20, 26),
            foreground: Rgb(220, 220, 220),
            palette: std::array::from_fn(|i| {
                Some(Rgb(i as u8 * 15, i as u8 * 10, i as u8 * 5))
            }),
        };
        let p = MarkdownPalette::from_terminal_colors(&colors);
        // repl_fence follows ANSI 5 (magenta)
        assert_eq!(p.repl_fence, "\x1b[38;2;75;50;25m");
        // diff_del follows ANSI 1 (red)
        assert_eq!(p.diff_del, "\x1b[38;2;15;10;5m");
    }

    #[test]
    fn from_terminal_colors_falls_back_when_palette_missing() {
        let colors = TerminalColors {
            background: Rgb(0, 0, 0),
            foreground: Rgb(255, 255, 255),
            palette: [None; 16],
        };
        let p = MarkdownPalette::from_terminal_colors(&colors);
        let default = MarkdownPalette::default_const();
        assert_eq!(p.repl_fence, default.repl_fence);
        assert_eq!(p.out_err, default.out_err);
    }
}
