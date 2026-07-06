use std::str::FromStr as _;
use std::sync::LazyLock;
use syntect::highlighting::{
    Color, FontStyle, ScopeSelectors, StyleModifier, Theme as SyntaxTheme, ThemeItem, ThemeSet,
    ThemeSettings,
};
use syntect::parsing::SyntaxSet;

use crate::palette::{Rgb, TerminalColors};
use crate::theme::Theme;

// Basic ANSI controls — these reference the terminal's own palette (indices
// 0/1/3/4/6 etc.), so they already follow the user's terminal theme.
pub(super) const RESET: &str = "\x1b[0m";
pub(super) const BOLD: &str = "\x1b[1m";
pub(super) const ITALIC: &str = "\x1b[3m";
pub(super) const UNDERLINE: &str = "\x1b[4m";

pub(super) static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);

/// Fallback syntect theme (base16-ocean.dark), used when the terminal doesn't
/// report its colors.
pub(super) static FALLBACK_THEME: LazyLock<SyntaxTheme> = LazyLock::new(|| {
    ThemeSet::load_defaults()
        .themes
        .remove("base16-ocean.dark")
        .expect("bundled theme")
});

/// Truecolor ANSI escapes for markdown/output highlighting, derived from the
/// unified `Theme`.  `default()` returns the previous hardcoded values
/// (base16-ocean family) as the fallback.
#[derive(Clone)]
pub struct MarkdownPalette {
    /// Heading text (theme accent — probed truecolor, not raw ANSI 36).
    pub heading: String,
    /// Inline code spans (theme secondary accent).
    pub inline_code: String,
    /// Tertiary text: language labels, truncation hints.
    pub muted: String,
    /// Ghost text: rules, fence borders, quote indents — structural marks.
    pub ghost: String,
    pub repl_fence: String,
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
    pub syntax_theme: SyntaxTheme,
}

impl MarkdownPalette {
    /// Previous hardcoded values (base16-ocean family).
    pub fn default_const() -> Self {
        Self {
            heading: s("\x1b[36m"),
            inline_code: s("\x1b[35m"),
            muted: s("\x1b[38;2;120;128;148m"),
            ghost: s("\x1b[38;2;80;86;104m"),
            repl_fence: s("\x1b[38;2;180;140;255m"),
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

    /// Derive palette from the unified `Theme` (which was itself generated from
    /// the terminal's reported colors).  This replaces the previous
    /// `from_terminal_colors` which independently mapped ANSI indices — now
    /// there is one mapping in `generate_theme` and this just formats the
    /// `Theme`'s `Rgb` fields as truecolor escapes.
    pub fn from_theme(t: &Theme) -> Self {
        Self {
            heading: fg_escape(t.accent),
            inline_code: fg_escape(t.accent_2),
            muted: fg_escape(t.text_tertiary),
            ghost: fg_escape(t.text_ghost),
            repl_fence: fg_escape(t.md_repl_fence),
            marker_accent: fg_escape(t.md_marker_accent),
            task_done: fg_escape(t.md_task_done),
            quote_bar: fg_escape(t.md_quote_bar),
            subhead: fg_escape(t.md_subhead),
            out_base: fg_escape(t.md_out_base),
            out_err: fg_escape(t.md_out_err),
            out_warn: fg_escape(t.md_out_warn),
            out_ok: fg_escape(t.md_out_ok),
            out_path: fg_escape(t.md_out_path),
            out_num: fg_escape(t.md_out_num),
            out_str: fg_escape(t.md_out_str),
            out_url: fg_escape(t.md_out_url),
            diff_add: fg_escape(t.md_diff_add),
            diff_del: fg_escape(t.md_diff_del),
            syntax_theme: generate_syntax_theme(t),
        }
    }

    /// Derive palette from terminal-reported colors (OSC 10/11/4).
    ///
    /// This is kept for backward compatibility — it now delegates to
    /// `generate_theme` + `from_theme` so there is a single ANSI→role mapping.
    pub fn from_terminal_colors(colors: &TerminalColors) -> Self {
        Self::from_theme(&crate::theme::generate_theme(colors))
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

fn fg_escape(c: Rgb) -> String {
    format!("\x1b[38;2;{};{};{}m", c.0, c.1, c.2)
}

/// Build a syntect `Theme` from the unified `Theme`'s syntax colors, mapping
/// each role to syntax scopes — mirroring opencode's `getSyntaxRules`.
pub(super) fn generate_syntax_theme(t: &Theme) -> SyntaxTheme {
    let fg = to_color(t.foreground);
    let bg = to_color(t.background);

    let items = syntax_items(SyntaxColors {
        comment: to_color(t.syntax_comment),
        keyword: to_color(t.syntax_keyword),
        string: to_color(t.syntax_string),
        number: to_color(t.syntax_number),
        function: to_color(t.syntax_function),
        type_: to_color(t.syntax_type),
        operator: to_color(t.syntax_operator),
        punctuation: to_color(t.syntax_punctuation),
        variable: to_color(t.syntax_variable),
        builtin: to_color(t.syntax_builtin),
    });

    SyntaxTheme {
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
    fn from_theme_maps_ansi_roles() {
        let colors = TerminalColors {
            background: Rgb(20, 20, 26),
            foreground: Rgb(220, 220, 220),
            palette: std::array::from_fn(|i| Some(Rgb(i as u8 * 15, i as u8 * 10, i as u8 * 5))),
        };
        let theme = crate::theme::generate_theme(&colors);
        let p = MarkdownPalette::from_theme(&theme);
        // repl_fence follows ANSI 5 (magenta)
        assert_eq!(p.repl_fence, "\x1b[38;2;75;50;25m");
        // diff_del follows ANSI 1 (red)
        assert_eq!(p.diff_del, "\x1b[38;2;15;10;5m");
    }

    #[test]
    fn from_terminal_colors_still_works() {
        let colors = TerminalColors {
            background: Rgb(20, 20, 26),
            foreground: Rgb(220, 220, 220),
            palette: std::array::from_fn(|i| Some(Rgb(i as u8 * 15, i as u8 * 10, i as u8 * 5))),
        };
        let p = MarkdownPalette::from_terminal_colors(&colors);
        assert_eq!(p.repl_fence, "\x1b[38;2;75;50;25m");
        assert_eq!(p.diff_del, "\x1b[38;2;15;10;5m");
    }

    #[test]
    fn syntax_theme_maps_ansi_palette_to_scopes() {
        let colors = TerminalColors {
            background: Rgb(20, 20, 26),
            foreground: Rgb(220, 220, 220),
            palette: std::array::from_fn(|i| Some(Rgb(i as u8 * 15, i as u8 * 10, i as u8 * 5))),
        };
        let theme = crate::theme::generate_theme(&colors);
        let syntect_theme = generate_syntax_theme(&theme);
        assert_eq!(
            syntect_theme.settings.foreground,
            Some(Color {
                r: 220,
                g: 220,
                b: 220,
                a: 0xff
            })
        );
        assert_eq!(
            syntect_theme.settings.background,
            Some(Color {
                r: 20,
                g: 20,
                b: 26,
                a: 0xff
            })
        );
        // Comment scope should exist and use bright-black (ANSI 8)
        let comment = syntect_theme
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
