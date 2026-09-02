//! Chrome prompt symbol: Nerd Font glyph with an ASCII fallback.
//!
//! Nerd Font glyphs are confined to the editor prompt chevron — the rest of
//! the UI draws from the unified plain-Unicode vocabulary in
//! `phi_tui::symbols`. Terminal capability queries cannot detect whether a
//! Nerd Font is installed, so detection is manual: set `PHI_NO_NERD_FONTS=1`
//! to fall back to the plain ASCII equivalent.

/// Editor prompt chevron (Nerd Font).
pub(crate) const NERD_SUCCESS_SYMBOL: &str = "\u{f013e}";
/// Editor prompt chevron (ASCII fallback).
pub(crate) const ASCII_SUCCESS_SYMBOL: &str = ">_";

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

pub(crate) fn success_symbol() -> &'static str {
    if nerd_fonts_enabled() {
        NERD_SUCCESS_SYMBOL
    } else {
        ASCII_SUCCESS_SYMBOL
    }
}
