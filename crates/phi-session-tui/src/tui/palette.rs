//! Inline TUI palette, now derived from the unified `phi_tui::Theme`.
//!
//! All ANSI→role mapping lives in `phi_tui::generate_theme`.  This module just
//! formats the `Theme`'s `Rgb` fields as ANSI escape strings for the inline TUI.

use std::collections::BTreeMap;

use phi_ext::{Rgb as ExtRgb, TerminalColorsProbe, ThemeSpec};
use phi_tui::theme::Theme;
use phi_tui::{MarkdownPalette, Rgb, TerminalColors};
use phi_tui::{apply_palette, bg_escape, bold_fg_escape, fg_escape, generate_theme};

pub struct InlinePalette {
    /// Warm gold for tool verbs.
    pub tool_accent: String,
    /// Bold text color for tool call header (pi: `toolTitle` = bold text).
    pub tool_title_fg: String,
    /// Gray for tool output text (pi: `toolOutput` = gray/muted).
    pub tool_output_fg: String,
    /// Violet for sub-LLM/rlm activity.
    pub subcall_accent: String,
    /// Soft blue for path-like args.
    pub path_fg: String,
    /// Muted green for quoted-string-like args.
    pub string_fg: String,
    /// Violet accent for numbers/durations.
    pub num_fg: String,
    /// User-echo text.
    pub user_fg: String,
    /// Muted text.
    pub muted_fg: String,
    /// Secondary text — one luminance step below primary (tool output bodies).
    pub text_secondary: String,
    /// Tertiary text — annotations, hints, truncation notices, notices.
    pub text_tertiary: String,
    /// Ghost text — structural marks: rules, placeholders, pager footers.
    pub text_ghost: String,
    /// Assistant prose.
    pub assistant_fg: String,
    /// REPL fence / panel color.
    pub panel_repl: String,
    /// Tool block background while pending (pi: `toolPendingBg`). Pending is
    /// the ONLY tinted tool state — completed cards render flat with a
    /// colored status glyph.
    pub tool_pending_bg: String,
    /// REPL block background — a neutral tint for the code-execution container
    /// that wraps tool/subcall cards (pi: no direct equivalent; pi has no
    /// REPL block concept, but the visual model is a Box(1,1,bgFn) like the
    /// tool cards, just with a neutral bg).
    pub repl_bg: String,
    /// Nested tool block background while pending — slightly brighter than
    /// `tool_pending_bg` so a tool card inside a REPL block stands out from
    /// the enclosing container.
    pub tool_pending_bg_nested: String,
    /// Chrome (status/footer) border.
    pub chrome_border: String,
    /// Chrome header: directory / branch / prompt glyph.
    pub chrome_dir_fg: String,
    /// Chrome header: model name.
    pub chrome_model_fg: String,
    /// Per-level thinking colors (progression: gray → blue → cyan → magenta).
    pub thinking_off: String,
    pub thinking_minimal: String,
    pub thinking_low: String,
    pub thinking_medium: String,
    pub thinking_high: String,
    pub thinking_xhigh: String,
    /// Error text accent (ANSI red, terminal-derived).
    pub error_fg: String,
    /// Warning text accent (ANSI yellow, terminal-derived).
    pub warning_fg: String,
    /// Inverted bg for the input-box separator, mirroring phi-mux's statusbar bg.
    pub statusbar_bg: String,
    /// Bright accent color (raw RGB) for the Knight Rider scanner.
    /// Also serves as the "from" end of the thinking-equalizer gradient
    /// (ANSI 6 / cyan, matching phi-mux's `accent`).
    pub spinner_accent: Rgb,
    /// Secondary accent (raw RGB, ANSI 5 / magenta) — the "to" end of the
    /// thinking-equalizer gradient, matching phi-mux's `accent_2`.
    pub accent_2: Rgb,
    /// Shimmer accent (raw RGB, ANSI 10 / bright green) — the SUCCESS side
    /// of the state arc: the completion flash and the ✓ glyph. Never used
    /// for in-progress surfaces.
    pub shimmer_accent: Rgb,
    /// Working shimmer (raw RGB, ANSI 5 / magenta family) — the IN-PROGRESS
    /// side of the state arc: tool-header shimmer, chrome shimmer while a
    /// run is in flight. The magenta→green hue cross at completion is what
    /// makes "done" legible without reading text.
    pub working_shimmer: Rgb,
    /// Success glyph color escape (bright green) for ✓ marks.
    pub success_fg: String,
    /// Working color as an escape string (in-flight spinner glyphs).
    pub working_fg: String,
    /// Primary accent as an escape string (user-turn gutter, focus marks).
    pub accent_fg: String,
    /// Light glint (raw RGB, terminal foreground) — the shimmer overlay for
    /// the thinking gradient: a light sweep over the cyan→magenta ramp
    /// rather than a hue flash. Hue changes are reserved for state changes.
    pub glint: Rgb,
    /// Terminal background (raw RGB), used to blend the scanner trail.
    pub terminal_bg: Rgb,
    /// Markdown/output highlighting palette.
    pub markdown: MarkdownPalette,
}

impl InlinePalette {
    /// Previous hardcoded values (base16-ocean family).
    pub fn default_const() -> Self {
        Self {
            tool_accent: s("\x1b[1m\x1b[38;2;255;210;120m"),
            tool_title_fg: s("\x1b[1m\x1b[37m"),
            tool_output_fg: s("\x1b[38;2;120;128;148m"),
            subcall_accent: s("\x1b[1m\x1b[38;2;180;140;255m"),
            path_fg: s("\x1b[38;2;130;180;230m"),
            string_fg: s("\x1b[38;2;150;190;150m"),
            num_fg: s("\x1b[38;2;170;150;230m"),
            user_fg: s("\x1b[38;2;190;200;215m"),
            muted_fg: s("\x1b[38;2;120;128;148m"),
            text_secondary: s("\x1b[38;2;177;180;190m"),
            text_tertiary: s("\x1b[38;2;130;134;146m"),
            text_ghost: s("\x1b[38;2;83;86;96m"),
            assistant_fg: s("\x1b[38;2;220;224;235m"),
            panel_repl: s("\x1b[38;2;180;140;255m"),
            tool_pending_bg: s("\x1b[48;2;40;40;50m"),
            repl_bg: s("\x1b[48;2;36;36;44m"),
            tool_pending_bg_nested: s("\x1b[48;2;48;48;58m"),
            chrome_border: s("\x1b[38;2;82;88;110m"),
            // Weight is a scarce resource: the model name is the single bold
            // chrome datum; every other label carries hierarchy via color.
            chrome_dir_fg: s("\x1b[92m"),
            chrome_model_fg: s("\x1b[1m\x1b[95m"),
            thinking_off: s("\x1b[38;2;80;80;80m"),
            thinking_minimal: s("\x1b[38;2;110;110;110m"),
            thinking_low: s("\x1b[38;2;95;135;175m"),
            thinking_medium: s("\x1b[38;2;129;162;190m"),
            thinking_high: s("\x1b[38;2;178;148;187m"),
            thinking_xhigh: s("\x1b[38;2;209;131;232m"),
            error_fg: s("\x1b[31m"),
            warning_fg: s("\x1b[33m"),
            statusbar_bg: s("\x1b[48;2;45;44;53m"),
            spinner_accent: Rgb(100, 200, 220),
            accent_2: Rgb(180, 140, 255),
            shimmer_accent: Rgb(80, 250, 120),
            working_shimmer: Rgb(180, 140, 255),
            success_fg: s("\x1b[92m"),
            working_fg: s("\x1b[38;2;180;140;255m"),
            accent_fg: s("\x1b[36m"),
            glint: Rgb(220, 224, 235),
            terminal_bg: Rgb(20, 20, 26),
            markdown: MarkdownPalette::default_const(),
        }
    }

    /// Derive from the unified `Theme` — the single ANSI→role mapping lives
    /// in `phi_tui::generate_theme`.  This just formats the `Theme`'s `Rgb`
    /// fields as ANSI escape strings.
    pub fn from_theme(t: &Theme) -> Self {
        Self {
            tool_accent: bold_fg_escape(t.tool_accent),
            tool_title_fg: bold_fg_escape(t.tool_title),
            tool_output_fg: fg_escape(t.tool_output),
            subcall_accent: bold_fg_escape(t.subcall_accent),
            path_fg: fg_escape(t.path),
            string_fg: fg_escape(t.string),
            num_fg: fg_escape(t.number),
            user_fg: fg_escape(t.user_fg),
            muted_fg: fg_escape(t.muted),
            text_secondary: fg_escape(t.text_secondary),
            text_tertiary: fg_escape(t.text_tertiary),
            text_ghost: fg_escape(t.text_ghost),
            assistant_fg: fg_escape(t.assistant_fg),
            panel_repl: fg_escape(t.panel_repl),
            tool_pending_bg: bg_escape(t.tool_pending_bg),
            repl_bg: bg_escape(t.repl_bg),
            tool_pending_bg_nested: bg_escape(t.tool_pending_bg_nested),
            // The theme's `border` role (gray ramp), not `muted` text gray —
            // the separator is felt as a divider, not read as content.
            chrome_border: fg_escape(t.border),
            // Weight is a scarce resource: the model name is the single bold
            // chrome datum; every other label carries hierarchy via color.
            chrome_dir_fg: fg_escape(t.chrome_dir),
            chrome_model_fg: bold_fg_escape(t.chrome_model),
            thinking_off: fg_escape(t.thinking_off),
            thinking_minimal: fg_escape(t.thinking_minimal),
            thinking_low: fg_escape(t.thinking_low),
            thinking_medium: fg_escape(t.thinking_medium),
            thinking_high: fg_escape(t.thinking_high),
            thinking_xhigh: fg_escape(t.thinking_xhigh),
            error_fg: fg_escape(t.error),
            warning_fg: fg_escape(t.warning),
            statusbar_bg: bg_escape(t.statusbar_bg),
            spinner_accent: t.accent,
            accent_2: t.accent_2,
            shimmer_accent: t.running,
            working_shimmer: t.accent_2,
            success_fg: fg_escape(t.running),
            working_fg: fg_escape(t.accent_2),
            accent_fg: fg_escape(t.accent),
            glint: t.foreground,
            terminal_bg: t.background,
            markdown: MarkdownPalette::from_theme(t),
        }
    }

    /// Derive from terminal-reported colors (OSC 10/11/4).
    ///
    /// Kept for backward compatibility — now delegates through `generate_theme`
    /// + `from_theme` so there is a single ANSI→role mapping.
    pub fn from_terminal_colors(colors: &TerminalColors) -> Self {
        Self::from_theme(&generate_theme(colors))
    }

    /// Derive from an extension-registered [`ThemeSpec`], mirroring the mux's
    /// `ScenePalette::from_extension_theme`: resolve the spec against the
    /// terminal's reported colours, overlay the resolved roles onto the base
    /// `generate_theme` derivation (so partial palettes fall back per-role),
    /// and format the result as the inline TUI's escape strings.
    pub fn from_extension_theme(spec: &ThemeSpec, colors: &TerminalColors) -> Self {
        let probe = TerminalColorsProbe {
            background: ExtRgb(colors.background.0, colors.background.1, colors.background.2),
            foreground: ExtRgb(colors.foreground.0, colors.foreground.1, colors.foreground.2),
            palette: colors.palette.map(|opt| opt.map(|c| ExtRgb(c.0, c.1, c.2))),
        };
        let resolved = spec.resolve(Some(&probe));
        let mut theme = generate_theme(colors);
        let palette: BTreeMap<String, Rgb> = resolved
            .iter()
            .map(|(name, c)| (name.clone(), Rgb(c.0, c.1, c.2)))
            .collect();
        apply_palette(&mut theme, &palette);
        Self::from_theme(&theme)
    }
}

impl Default for InlinePalette {
    fn default() -> Self {
        Self::default_const()
    }
}

pub(crate) fn s(v: &str) -> String {
    v.to_owned()
}
