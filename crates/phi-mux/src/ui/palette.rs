//! Palette types for the compositor scene.

use crate::grid::color::Color;
use crate::grid::theme;
use phi_tui::palette::TerminalColors;

#[derive(Clone, Copy)]
pub struct ScenePalette {
    pub fg: Color,
    pub bg: Color,
    pub surface_raised: Color,
    pub statusbar_bg: Color,
    pub statusbar_fg: Color,
    pub sidebar_bg: Color,
    pub border: Color,
    pub muted: Color,
    pub term_fg: Color,
    pub term_bg: Color,
    pub accent: Color,
    pub accent_2: Color,

    pub ansi: [Color; 16],

    pub running: Color,
    pub success: Color,
    pub success_subtle: Color,
    pub warning: Color,
    pub error: Color,
}

impl Default for ScenePalette {
    fn default() -> Self {
        // Unify every path through `generate_system` so there is no separate
        // hardcoded fallback — the statusbar, surfaces, borders, and accents
        // always come from the same gray ramp / ANSI mapping. This is what
        // tests get; production goes through `from_terminal_colors` in app.rs.
        Self::from_terminal_colors(&theme::default_terminal_colors())
    }
}

impl ScenePalette {
    /// Derive a palette from the host terminal's reported colors (OSC 10/11/4),
    /// matching opencode's auto-theming. Missing palette entries fall back to
    /// the standard 256-color cube inside `theme::generate_system`.
    pub fn from_terminal_colors(colors: &TerminalColors) -> Self {
        let g = theme::generate_system(colors);
        Self {
            fg: g.fg,
            bg: theme::TRANSPARENT,
            surface_raised: g.surface_raised,
            statusbar_bg: g.statusbar_bg,
            statusbar_fg: g.statusbar_fg,
            sidebar_bg: theme::TRANSPARENT,
            border: g.border,
            muted: g.muted,
            term_fg: g.term_fg,
            term_bg: theme::TRANSPARENT,
            accent: g.accent,
            accent_2: g.accent_2,
            ansi: g.ansi,
            running: g.running,
            success: g.success,
            success_subtle: g.success_subtle,
            warning: g.warning,
            error: g.error,
        }
    }
}
