//! Inline TUI palette, now derived from the unified `phi_tui::Theme`.
//!
//! All ANSI→role mapping lives in `phi_tui::generate_theme`.  This module just
//! formats the `Theme`'s `Rgb` fields as ANSI escape strings for the inline TUI.

use phi_tui::theme::Theme;
use phi_tui::{MarkdownPalette, Rgb, TerminalColors};
use phi_tui::{bg_escape, bold_fg_escape, fg_escape, generate_theme};

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
    /// Assistant prose.
    pub assistant_fg: String,
    /// REPL fence / panel color.
    pub panel_repl: String,
    /// User message full-row background (pi: `userMessageBg`).
    pub user_message_bg: String,
    /// Tool block background while pending (pi: `toolPendingBg`).
    pub tool_pending_bg: String,
    /// Tool block background on success (pi: `toolSuccessBg`).
    pub tool_success_bg: String,
    /// Tool block background on error (pi: `toolErrorBg`).
    pub tool_error_bg: String,
    /// REPL block background — a neutral tint for the code-execution container
    /// that wraps tool/subcall cards (pi: no direct equivalent; pi has no
    /// REPL block concept, but the visual model is a Box(1,1,bgFn) like the
    /// tool cards, just with a neutral bg).
    pub repl_bg: String,
    /// Chrome (status/footer) border.
    pub chrome_border: String,
    /// Chrome header: agent label + usage (phi, tokens).
    pub chrome_agent_fg: String,
    /// Chrome header: cost figure.
    pub chrome_cost_fg: String,
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
    /// Shimmer accent (raw RGB, ANSI 10 / bright green) — the running status
    /// color blended onto the thinking gradient to create the
    /// slide-and-cycle shimmer, matching phi-mux's `running`.
    pub shimmer_accent: Rgb,
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
            assistant_fg: s("\x1b[38;2;220;224;235m"),
            panel_repl: s("\x1b[38;2;180;140;255m"),
            user_message_bg: s("\x1b[48;2;52;53;65m"),
            tool_pending_bg: s("\x1b[48;2;40;40;50m"),
            tool_success_bg: s("\x1b[48;2;40;50;40m"),
            tool_error_bg: s("\x1b[48;2;60;40;40m"),
            repl_bg: s("\x1b[48;2;36;36;44m"),
            chrome_border: s("\x1b[38;2;82;88;110m"),
            chrome_agent_fg: s("\x1b[1m\x1b[37m"),
            chrome_cost_fg: s("\x1b[1m\x1b[32m"),
            chrome_dir_fg: s("\x1b[1m\x1b[92m"),
            chrome_model_fg: s("\x1b[1m\x1b[95m"),
            thinking_off: s("\x1b[1m\x1b[38;2;80;80;80m"),
            thinking_minimal: s("\x1b[1m\x1b[38;2;110;110;110m"),
            thinking_low: s("\x1b[1m\x1b[38;2;95;135;175m"),
            thinking_medium: s("\x1b[1m\x1b[38;2;129;162;190m"),
            thinking_high: s("\x1b[1m\x1b[38;2;178;148;187m"),
            thinking_xhigh: s("\x1b[1m\x1b[38;2;209;131;232m"),
            error_fg: s("\x1b[31m"),
            warning_fg: s("\x1b[33m"),
            statusbar_bg: s("\x1b[48;2;45;44;53m"),
            spinner_accent: Rgb(100, 200, 220),
            accent_2: Rgb(180, 140, 255),
            shimmer_accent: Rgb(80, 250, 120),
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
            assistant_fg: fg_escape(t.assistant_fg),
            panel_repl: fg_escape(t.panel_repl),
            user_message_bg: bg_escape(t.user_message_bg),
            tool_pending_bg: bg_escape(t.tool_pending_bg),
            tool_success_bg: bg_escape(t.tool_success_bg),
            tool_error_bg: bg_escape(t.tool_error_bg),
            repl_bg: bg_escape(t.repl_bg),
            chrome_border: fg_escape(t.muted),
            chrome_agent_fg: bold_fg_escape(t.chrome_agent),
            chrome_cost_fg: bold_fg_escape(t.chrome_cost),
            chrome_dir_fg: bold_fg_escape(t.chrome_dir),
            chrome_model_fg: bold_fg_escape(t.chrome_model),
            thinking_off: bold_fg_escape(t.thinking_off),
            thinking_minimal: bold_fg_escape(t.thinking_minimal),
            thinking_low: bold_fg_escape(t.thinking_low),
            thinking_medium: bold_fg_escape(t.thinking_medium),
            thinking_high: bold_fg_escape(t.thinking_high),
            thinking_xhigh: bold_fg_escape(t.thinking_xhigh),
            error_fg: fg_escape(t.error),
            warning_fg: fg_escape(t.warning),
            statusbar_bg: bg_escape(t.statusbar_bg),
            spinner_accent: t.accent,
            accent_2: t.accent_2,
            shimmer_accent: t.running,
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
}

impl Default for InlinePalette {
    fn default() -> Self {
        Self::default_const()
    }
}

pub(crate) fn s(v: &str) -> String {
    v.to_owned()
}
