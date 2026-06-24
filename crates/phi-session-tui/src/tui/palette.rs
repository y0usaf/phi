//! Inline TUI palette and terminal-derived colors.

use phi_tui::MarkdownPalette;

pub struct InlinePalette {
    /// Warm gold for tool verbs.
    pub tool_accent: String,
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
    /// User message border.
    pub user_border: String,
    /// Assistant message border.
    pub assistant_border: String,
    /// Chrome (status/footer) border.
    pub chrome_border: String,
    /// Chrome header: agent label + usage (PHI, tokens).
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
    /// Markdown/output highlighting palette.
    pub markdown: MarkdownPalette,
}

impl InlinePalette {
    /// Previous hardcoded values (base16-ocean family).
    pub fn default_const() -> Self {
        Self {
            tool_accent: s("\x1b[1m\x1b[38;2;255;210;120m"),
            subcall_accent: s("\x1b[1m\x1b[38;2;180;140;255m"),
            path_fg: s("\x1b[38;2;130;180;230m"),
            string_fg: s("\x1b[38;2;150;190;150m"),
            num_fg: s("\x1b[38;2;170;150;230m"),
            user_fg: s("\x1b[38;2;190;200;215m"),
            muted_fg: s("\x1b[38;2;120;128;148m"),
            assistant_fg: s("\x1b[38;2;220;224;235m"),
            panel_repl: s("\x1b[38;2;180;140;255m"),
            user_border: s("\x1b[38;2;96;122;196m"),
            assistant_border: s("\x1b[38;2;84;91;116m"),
            chrome_border: s("\x1b[38;2;82;88;110m"),
            chrome_agent_fg: s("\x1b[1m\x1b[37m"),
            chrome_cost_fg: s("\x1b[1m\x1b[32m"),
            chrome_dir_fg: s("\x1b[1m\x1b[92m"),
            chrome_model_fg: s("\x1b[95m"),
            thinking_off: s("\x1b[38;2;80;80;80m"),
            thinking_minimal: s("\x1b[38;2;110;110;110m"),
            thinking_low: s("\x1b[38;2;95;135;175m"),
            thinking_medium: s("\x1b[38;2;129;162;190m"),
            thinking_high: s("\x1b[38;2;178;148;187m"),
            thinking_xhigh: s("\x1b[38;2;209;131;232m"),
            error_fg: s("\x1b[31m"),
            warning_fg: s("\x1b[33m"),
            statusbar_bg: s("\x1b[48;2;45;44;53m"),
            markdown: MarkdownPalette::default_const(),
        }
    }

    /// Derive from terminal-reported colors (OSC 10/11/4), mirroring opencode's
    /// `generateSystem`: ANSI palette → UI roles.
    pub fn from_terminal_colors(colors: &phi_tui::TerminalColors) -> Self {
        let col = |i: usize| {
            colors.palette[i].map(|c| format!("\x1b[38;2;{};{};{}m", c.0, c.1, c.2))
        };
        let col_bold = |i: usize| {
            colors.palette[i]
                .map(|c| format!("\x1b[1m\x1b[38;2;{};{};{}m", c.0, c.1, c.2))
        };
        let fallback = Self::default_const();
        Self {
            tool_accent: col_bold(3).unwrap_or_else(|| fallback.tool_accent.clone()),
            subcall_accent: col_bold(5).unwrap_or_else(|| fallback.subcall_accent.clone()),
            path_fg: col(4).unwrap_or_else(|| fallback.path_fg.clone()),
            string_fg: col(2).unwrap_or_else(|| fallback.string_fg.clone()),
            num_fg: col(3).unwrap_or_else(|| fallback.num_fg.clone()),
            user_fg: col(7).unwrap_or_else(|| fallback.user_fg.clone()),
            muted_fg: col(8).unwrap_or_else(|| fallback.muted_fg.clone()),
            assistant_fg: col(7).unwrap_or_else(|| fallback.assistant_fg.clone()),
            panel_repl: col(5).unwrap_or_else(|| fallback.panel_repl.clone()),
            user_border: col(4).unwrap_or_else(|| fallback.user_border.clone()),
            assistant_border: col(8).unwrap_or_else(|| fallback.assistant_border.clone()),
            chrome_border: col(8).unwrap_or_else(|| fallback.chrome_border.clone()),
            chrome_agent_fg: col_bold(7).unwrap_or_else(|| fallback.chrome_agent_fg.clone()),
            chrome_cost_fg: col_bold(2).unwrap_or_else(|| fallback.chrome_cost_fg.clone()),
            chrome_dir_fg: col_bold(10).unwrap_or_else(|| fallback.chrome_dir_fg.clone()),
            chrome_model_fg: col(13).unwrap_or_else(|| fallback.chrome_model_fg.clone()),
            thinking_off: col(8).unwrap_or_else(|| fallback.thinking_off.clone()),
            thinking_minimal: col(7).unwrap_or_else(|| fallback.thinking_minimal.clone()),
            thinking_low: col(4).unwrap_or_else(|| fallback.thinking_low.clone()),
            thinking_medium: col(6).unwrap_or_else(|| fallback.thinking_medium.clone()),
            thinking_high: col(5).unwrap_or_else(|| fallback.thinking_high.clone()),
            thinking_xhigh: col(13).unwrap_or_else(|| fallback.thinking_xhigh.clone()),
            error_fg: col(1).unwrap_or_else(|| fallback.error_fg.clone()),
            warning_fg: col(3).unwrap_or_else(|| fallback.warning_fg.clone()),
            statusbar_bg: statusbar_bg_from_terminal(colors, &fallback.statusbar_bg),
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


// ── Forgecode-style chrome symbols ──────────────────────────────────────
//
// The chrome prompt uses Nerd Font glyphs (folder, branch, chevron, etc.)
// to match the reference forgecode prompt.  Terminal capability queries
// cannot detect whether a Nerd Font is installed, so detection is manual:
// set `PHI_NO_NERD_FONTS=1` to fall back to plain ASCII equivalents.
//
// Nerd Font symbols — left prompt
pub(crate) const NERD_DIR_SYMBOL: &str = "\u{ea83}";      // folder icon
pub(crate) const NERD_BRANCH_SYMBOL: &str = "\u{f418}"; //  branch icon
pub(crate) const NERD_SUCCESS_SYMBOL: &str = "\u{f013e}"; //  chevron

// Nerd Font symbols — right prompt
pub(crate) const NERD_AGENT_SYMBOL: &str = "\u{f167a}";
pub(crate) const NERD_MODEL_SYMBOL: &str = "\u{ec19}";
pub(crate) const NERD_COST_SYMBOL: &str = "\u{f155}"; // dollar sign

// ASCII fallbacks
pub(crate) const ASCII_DIR_SYMBOL: &str = "./";
pub(crate) const ASCII_BRANCH_SYMBOL: &str = "#";
pub(crate) const ASCII_SUCCESS_SYMBOL: &str = ">";
pub(crate) const ASCII_AGENT_SYMBOL: &str = "@";
pub(crate) const ASCII_MODEL_SYMBOL: &str = "m";
pub(crate) const ASCII_COST_SYMBOL: &str = "$";

/// Width at which effort collapses from full label to 3-letter abbreviation.
pub(crate) const WIDE_TERMINAL_THRESHOLD: usize = 100;

/// Whether Nerd Font glyphs are enabled.  Detection is manual: terminals
/// don't expose font coverage, and cursor-position probing can't distinguish
/// a real glyph from a fallback box.  Set `PHI_NO_NERD_FONTS=1` for ASCII.
pub(crate) fn nerd_fonts_enabled() -> bool {
    !matches!(
        std::env::var("PHI_NO_NERD_FONTS").as_deref(),
        Ok("1") | Ok("true")
    )
}

pub(crate) fn dir_symbol() -> &'static str {
    if nerd_fonts_enabled() { NERD_DIR_SYMBOL } else { ASCII_DIR_SYMBOL }
}
pub(crate) fn branch_symbol() -> &'static str {
    if nerd_fonts_enabled() { NERD_BRANCH_SYMBOL } else { ASCII_BRANCH_SYMBOL }
}
pub(crate) fn success_symbol() -> &'static str {
    if nerd_fonts_enabled() { NERD_SUCCESS_SYMBOL } else { ASCII_SUCCESS_SYMBOL }
}
pub(crate) fn agent_symbol() -> &'static str {
    if nerd_fonts_enabled() { NERD_AGENT_SYMBOL } else { ASCII_AGENT_SYMBOL }
}
pub(crate) fn model_symbol() -> &'static str {
    if nerd_fonts_enabled() { NERD_MODEL_SYMBOL } else { ASCII_MODEL_SYMBOL }
}
pub(crate) fn cost_symbol() -> &'static str {
    if nerd_fonts_enabled() { NERD_COST_SYMBOL } else { ASCII_COST_SYMBOL }
}
