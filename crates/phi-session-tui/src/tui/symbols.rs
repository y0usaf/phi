//! Forgecode-style chrome symbols: Nerd Font glyphs and ASCII fallbacks.

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
