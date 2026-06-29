//! Inline TUI palette and terminal-derived colors.

use phi_tui::{MarkdownPalette, Rgb};

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

    /// Derive from terminal-reported colors (OSC 10/11/4), mirroring opencode's
    /// `generateSystem`: ANSI palette → UI roles.
    pub fn from_terminal_colors(colors: &phi_tui::TerminalColors) -> Self {
        let col =
            |i: usize| colors.palette[i].map(|c| format!("\x1b[38;2;{};{};{}m", c.0, c.1, c.2));
        let col_bold = |i: usize| {
            colors.palette[i].map(|c| format!("\x1b[1m\x1b[38;2;{};{};{}m", c.0, c.1, c.2))
        };
        let fallback = Self::default_const();
        Self {
            tool_accent: col_bold(3).unwrap_or_else(|| fallback.tool_accent.clone()),
            tool_title_fg: col_bold(7).unwrap_or_else(|| fallback.tool_title_fg.clone()),
            tool_output_fg: col(8).unwrap_or_else(|| fallback.tool_output_fg.clone()),
            subcall_accent: col_bold(5).unwrap_or_else(|| fallback.subcall_accent.clone()),
            path_fg: col(4).unwrap_or_else(|| fallback.path_fg.clone()),
            string_fg: col(2).unwrap_or_else(|| fallback.string_fg.clone()),
            num_fg: col(3).unwrap_or_else(|| fallback.num_fg.clone()),
            user_fg: col(7).unwrap_or_else(|| fallback.user_fg.clone()),
            muted_fg: col(8).unwrap_or_else(|| fallback.muted_fg.clone()),
            assistant_fg: col(7).unwrap_or_else(|| fallback.assistant_fg.clone()),
            panel_repl: col(5).unwrap_or_else(|| fallback.panel_repl.clone()),
            user_message_bg: bg_from_terminal(colors, 4, &fallback.user_message_bg),
            tool_pending_bg: bg_from_terminal(colors, 8, &fallback.tool_pending_bg),
            tool_success_bg: bg_from_terminal(colors, 2, &fallback.tool_success_bg),
            tool_error_bg: bg_from_terminal(colors, 1, &fallback.tool_error_bg),
            chrome_border: col(8).unwrap_or_else(|| fallback.chrome_border.clone()),
            chrome_agent_fg: col_bold(7).unwrap_or_else(|| fallback.chrome_agent_fg.clone()),
            chrome_cost_fg: col_bold(2).unwrap_or_else(|| fallback.chrome_cost_fg.clone()),
            chrome_dir_fg: col_bold(10).unwrap_or_else(|| fallback.chrome_dir_fg.clone()),
            chrome_model_fg: col_bold(13).unwrap_or_else(|| fallback.chrome_model_fg.clone()),
            thinking_off: col_bold(8).unwrap_or_else(|| fallback.thinking_off.clone()),
            thinking_minimal: col_bold(7).unwrap_or_else(|| fallback.thinking_minimal.clone()),
            thinking_low: col_bold(4).unwrap_or_else(|| fallback.thinking_low.clone()),
            thinking_medium: col_bold(6).unwrap_or_else(|| fallback.thinking_medium.clone()),
            thinking_high: col_bold(5).unwrap_or_else(|| fallback.thinking_high.clone()),
            thinking_xhigh: col_bold(13).unwrap_or_else(|| fallback.thinking_xhigh.clone()),
            error_fg: col(1).unwrap_or_else(|| fallback.error_fg.clone()),
            warning_fg: col(3).unwrap_or_else(|| fallback.warning_fg.clone()),
            statusbar_bg: statusbar_bg_from_terminal(colors, &fallback.statusbar_bg),
            spinner_accent: colors.palette[6].unwrap_or(fallback.spinner_accent),
            accent_2: colors.palette[5].unwrap_or(fallback.accent_2),
            shimmer_accent: colors.palette[10].unwrap_or(fallback.shimmer_accent),
            terminal_bg: colors.background,
            markdown: MarkdownPalette::from_terminal_colors(colors),
        }
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

/// Compute the statusbar bg escape from the terminal background, mirroring
/// phi-mux's `grays[3]` (the `statusbar_bg` slot in `GeneratedTheme`).
pub(crate) fn statusbar_bg_from_terminal(
    colors: &phi_tui::TerminalColors,
    _fallback: &str,
) -> String {
    let bg = colors.background;
    let lum = (u32::from(bg.0) * 299 + u32::from(bg.1) * 587 + u32::from(bg.2) * 114) / 1000;
    let dark = lum <= 127;

    // grays[3]: factor = 3/12 = 0.25
    let factor = 0.25_f64;
    let (r, g, b) = if dark {
        if lum < 10 {
            let v = (factor * 0.4 * 255.0).floor() as u8;
            (v, v, v)
        } else {
            let new_lum = f64::from(lum) + (255.0 - f64::from(lum)) * factor * 0.4;
            let ratio = new_lum / f64::from(lum);
            (
                (f64::from(bg.0) * ratio).min(255.0) as u8,
                (f64::from(bg.1) * ratio).min(255.0) as u8,
                (f64::from(bg.2) * ratio).min(255.0) as u8,
            )
        }
    } else if lum > 245 {
        let v = (255.0 - factor * 0.4 * 255.0).floor() as u8;
        (v, v, v)
    } else {
        let new_lum = f64::from(lum) * (1.0 - factor * 0.4);
        let ratio = new_lum / f64::from(lum);
        (
            (f64::from(bg.0) * ratio).max(0.0) as u8,
            (f64::from(bg.1) * ratio).max(0.0) as u8,
            (f64::from(bg.2) * ratio).max(0.0) as u8,
        )
    };
    format!("\x1b[48;2;{r};{g};{b}m")
}

/// Derive a background-color SGR from the terminal background, blended
/// toward an ANSI palette color. Mirrors pi's dark theme approach: the
/// background is a subtle tint of the terminal bg, not a flat color.
pub(crate) fn bg_from_terminal(
    colors: &phi_tui::TerminalColors,
    ansi_idx: usize,
    _fallback: &str,
) -> String {
    let bg = colors.background;
    let tint = colors.palette[ansi_idx].unwrap_or(phi_tui::Rgb(bg.0, bg.1, bg.2));
    // Blend 20% of the tint color into the terminal background for a
    // subtle wash, like pi's `userMsgBg` / `toolPendingBg` slots.
    let blend = |bg_c: u8, tint_c: u8| -> u8 {
        ((f64::from(bg_c) * 0.8) + (f64::from(tint_c) * 0.2)).round() as u8
    };
    let r = blend(bg.0, tint.0);
    let g = blend(bg.1, tint.1);
    let b = blend(bg.2, tint.2);
    format!("\x1b[48;2;{r};{g};{b}m")
}
