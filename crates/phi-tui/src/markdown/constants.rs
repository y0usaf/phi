use std::str::FromStr as _;
use std::sync::LazyLock;
use syntect::highlighting::{
    Color, FontStyle, ScopeSelectors, StyleModifier, Theme, ThemeItem, ThemeSet, ThemeSettings,
};
use syntect::parsing::SyntaxSet;

use crate::palette::{Rgb, TerminalColors};

// Basic ANSI controls — these reference the terminal's own palette (indices
// 0/1/3/4/6 etc.), so they already follow the user's terminal theme.
pub(super) const RESET: &str = "\x1b[0m";
pub(super) const BOLD: &str = "\x1b[1m";
pub(super) const DIM: &str = "\x1b[2m";
pub(super) const ITALIC: &str = "\x1b[3m";
pub(super) const UNDERLINE: &str = "\x1b[4m";
pub(super) const CYAN: &str = "\x1b[36m";

pub(super) static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);

/// Fallback syntect theme (base16-ocean.dark), used when the terminal doesn't
/// report its colors.
pub(super) static FALLBACK_THEME: LazyLock<Theme> = LazyLock::new(|| {
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
    /// Syntect theme for code syntax highlighting, derived from the terminal
    /// palette when available. Falls back to base16-ocean.dark.
    pub syntax_theme: Theme,
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
            syntax_theme: FALLBACK_THEME.clone(),
        }
    }

    /// Derive palette from terminal-reported colors (OSC 10/11/4), mirroring
    /// opencode's `generateSystem`: ANSI palette → markdown roles.
    pub fn from_terminal_colors(colors: &TerminalColors) -> Self {
        let col =
            |i: usize| colors.palette[i].map(|c| format!("\x1b[38;2;{};{};{}m", c.0, c.1, c.2));
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
            syntax_theme: generate_syntax_theme(colors),
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

/// Build a syntect `Theme` from the terminal's reported colors, mapping ANSI
/// palette entries to syntax scopes — mirroring opencode's `getSyntaxRules`.
/// When palette entries are missing, falls back to the terminal's default
/// foreground/background so code is always readable.
pub(super) fn generate_syntax_theme(colors: &TerminalColors) -> Theme {
    let fg = to_color(colors.foreground);
    let bg = to_color(colors.background);
    let ansi = |i: usize| colors.palette[i].map(to_color).unwrap_or(fg);

    let items = syntax_items(SyntaxColors {
        comment: ansi(8),
        keyword: ansi(5),
        string: ansi(2),
        number: ansi(3),
        function: ansi(4),
        type_: ansi(6),
        operator: ansi(6),
        punctuation: fg,
        variable: fg,
        builtin: ansi(1),
    });

    Theme {
        name: Some("phi-terminal".to_owned()),
        author: None,
        settings: ThemeSettings {
            foreground: Some(fg),
            background: Some(bg),
            ..ThemeSettings::default()
        },
        scopes: items,
    }
}

struct SyntaxColors {
    comment: Color,
    keyword: Color,
    string: Color,
    number: Color,
    function: Color,
    type_: Color,
    operator: Color,
    punctuation: Color,
    variable: Color,
    builtin: Color,
}

fn to_color(c: Rgb) -> Color {
    Color {
        r: c.0,
        g: c.1,
        b: c.2,
        a: 0xff,
    }
}

/// Build syntax scope rules from a color set, mirroring opencode's
/// `getSyntaxRules`. Each rule maps a TextMate scope selector to a style.
fn syntax_items(c: SyntaxColors) -> Vec<ThemeItem> {
    let fg = |color: Color| StyleModifier {
        foreground: Some(color),
        background: None,
        font_style: None,
    };
    let italic_fg = |color: Color| StyleModifier {
        foreground: Some(color),
        background: None,
        font_style: Some(FontStyle::ITALIC),
    };
    let bold_fg = |color: Color| StyleModifier {
        foreground: Some(color),
        background: None,
        font_style: Some(FontStyle::BOLD),
    };

    vec![
        item("comment", italic_fg(c.comment)),
        item("comment.documentation", italic_fg(c.comment)),
        item("string", fg(c.string)),
        item("string.symbol", fg(c.string)),
        item("number", fg(c.number)),
        item("boolean", fg(c.number)),
        item("constant", fg(c.number)),
        item("constant.numeric", fg(c.number)),
        item("character.special", fg(c.string)),
        item("keyword", italic_fg(c.keyword)),
        item("keyword.return", italic_fg(c.keyword)),
        item("keyword.conditional", italic_fg(c.keyword)),
        item("keyword.repeat", italic_fg(c.keyword)),
        item("keyword.type", bold_fg(c.type_)),
        item("keyword.function", fg(c.function)),
        item("keyword.import", fg(c.keyword)),
        item("keyword.operator", fg(c.operator)),
        item("keyword.directive", italic_fg(c.keyword)),
        item("keyword.modifier", italic_fg(c.keyword)),
        item("keyword.exception", italic_fg(c.keyword)),
        item("operator", fg(c.operator)),
        item("punctuation.delimiter", fg(c.operator)),
        item("punctuation", fg(c.punctuation)),
        item("punctuation.bracket", fg(c.punctuation)),
        item("variable", fg(c.variable)),
        item("variable.parameter", fg(c.variable)),
        item("variable.member", fg(c.function)),
        item("function", fg(c.function)),
        item("function.method", fg(c.function)),
        item("function.method.call", fg(c.variable)),
        item("function.call", fg(c.variable)),
        item("function.constructor", fg(c.function)),
        item("type", fg(c.type_)),
        item("type.builtin", fg(c.builtin)),
        item("module", fg(c.type_)),
        item("namespace", fg(c.type_)),
        item("class", fg(c.type_)),
        item("property", fg(c.variable)),
        item("field", fg(c.variable)),
        item("parameter", fg(c.variable)),
        item("variable.builtin", fg(c.builtin)),
        item("function.builtin", fg(c.builtin)),
        item("module.builtin", fg(c.builtin)),
        item("constant.builtin", fg(c.builtin)),
        item("variable.super", fg(c.builtin)),
        item("string.escape", fg(c.keyword)),
        item("string.regexp", fg(c.keyword)),
        item("punctuation.special", fg(c.operator)),
        item("keyword.export", fg(c.keyword)),
        item("attribute", bold_fg(c.number)),
        item("annotation", bold_fg(c.number)),
        item("tag", fg(c.builtin)),
        item("tag.attribute", fg(c.keyword)),
        item("tag.delimiter", fg(c.operator)),
    ]
}

fn item(scope: &str, style: StyleModifier) -> ThemeItem {
    ThemeItem {
        scope: ScopeSelectors::from_str(scope).expect("valid scope selector"),
        style,
    }
}

// Re-export so highlight.rs can use the palette's syntax theme without
// importing from `constants` directly.

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
            palette: std::array::from_fn(|i| Some(Rgb(i as u8 * 15, i as u8 * 10, i as u8 * 5))),
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

    #[test]
    fn syntax_theme_maps_ansi_palette_to_scopes() {
        let colors = TerminalColors {
            background: Rgb(20, 20, 26),
            foreground: Rgb(220, 220, 220),
            palette: std::array::from_fn(|i| Some(Rgb(i as u8 * 15, i as u8 * 10, i as u8 * 5))),
        };
        let theme = generate_syntax_theme(&colors);
        assert_eq!(
            theme.settings.foreground,
            Some(Color {
                r: 220,
                g: 220,
                b: 220,
                a: 0xff
            })
        );
        assert_eq!(
            theme.settings.background,
            Some(Color {
                r: 20,
                g: 20,
                b: 26,
                a: 0xff
            })
        );
        // Comment scope should exist and use bright-black (ANSI 8)
        let comment = theme
            .scopes
            .iter()
            .find(|s| s.scope == ScopeSelectors::from_str("comment").unwrap());
        assert!(comment.is_some(), "comment scope missing");
        // ANSI 8 = 8*15=120, 8*10=80, 8*5=40
        assert_eq!(
            comment.unwrap().style.foreground,
            Some(Color {
                r: 120,
                g: 80,
                b: 40,
                a: 0xff
            })
        );
    }
}
