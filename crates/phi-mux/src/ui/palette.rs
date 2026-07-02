//! Palette types for the compositor scene.
//!
//! `ScenePalette` is now a thin adapter over the unified `phi_tui::Theme` —
//! it converts `Rgb` values to `phi_grid::Color` so the mux compositor stays in
//! `Color` space.  All derivation logic lives in `phi_tui::generate_theme`.
//!
//! When an extension registers a [`ThemeSpec`], the mux resolves it and
//! overrides the corresponding roles in the generated [`Theme`], so
//! extensions can provide partial palettes that fall back to the
//! built-in derivation for any role they don't supply.

use std::collections::BTreeMap;

use crate::grid::color::Color;
use crate::grid::theme;
use phi_ext::{Rgb as ExtRgb, TerminalColorsProbe, ThemeSpec};
use phi_tui::palette::TerminalColors;
use phi_tui::theme::Theme;
use phi_tui::{Rgb, default_terminal_colors, generate_theme};

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

fn to_color(c: Rgb) -> Color {
    Color::rgb(c.0, c.1, c.2)
}

fn to_color_array(arr: [Rgb; 16]) -> [Color; 16] {
    let mut out = [Color::rgb(0, 0, 0); 16];
    for i in 0..16 {
        out[i] = to_color(arr[i]);
    }
    out
}

fn ext_to_tui_rgb(c: ExtRgb) -> Rgb {
    Rgb(c.0, c.1, c.2)
}

impl Default for ScenePalette {
    fn default() -> Self {
        // Unify every path through `generate_theme` so there is no separate
        // hardcoded fallback — the statusbar, surfaces, borders, and accents
        // always come from the same gray ramp / ANSI mapping. This is what
        // tests get; production goes through `from_terminal_colors` in app.rs.
        Self::from_terminal_colors(&default_terminal_colors())
    }
}

impl ScenePalette {
    /// Derive a palette from the host terminal's reported colors (OSC 10/11/4),
    /// matching opencode's auto-theming. Missing palette entries fall back to
    /// the standard 256-color cube inside `phi_tui::generate_theme`.
    pub fn from_terminal_colors(colors: &TerminalColors) -> Self {
        let t = generate_theme(colors);
        Self::from_theme(&t)
    }

    /// Build a `ScenePalette` from a unified `Theme`, converting `Rgb` → `Color`.
    pub fn from_theme(t: &Theme) -> Self {
        Self {
            fg: to_color(t.foreground),
            bg: theme::TRANSPARENT,
            surface_raised: to_color(t.surface_raised),
            statusbar_bg: to_color(t.statusbar_bg),
            statusbar_fg: to_color(t.foreground),
            sidebar_bg: theme::TRANSPARENT,
            border: to_color(t.border),
            muted: to_color(t.muted),
            term_fg: to_color(t.foreground),
            term_bg: theme::TRANSPARENT,
            accent: to_color(t.accent),
            accent_2: to_color(t.accent_2),
            ansi: to_color_array(t.ansi),
            running: to_color(t.running),
            success: to_color(t.success),
            success_subtle: to_color(t.success_subtle),
            warning: to_color(t.warning),
            error: to_color(t.error),
        }
    }

    /// Build a `ScenePalette` from an extension-registered [`ThemeSpec`].
    ///
    /// The spec is resolved (using its resolver closure if present, or its
    /// static palette map). The resolved roles are then overlaid onto a
    /// base `Theme` generated from the terminal's reported colours, so any
    /// role the extension doesn't provide falls back to the built-in
    /// derivation. This means an extension can override just the accent
    /// colour, or just the background, or provide a complete palette.
    ///
    /// `terminal_colors` is the host terminal's OSC-reported colours (or
    /// the default fallback). It is passed to the resolver so a
    /// programmatic theme can adapt to the terminal.
    pub fn from_extension_theme(spec: &ThemeSpec, terminal_colors: &TerminalColors) -> Self {
        let probe = TerminalColorsProbe {
            background: ExtRgb(
                terminal_colors.background.0,
                terminal_colors.background.1,
                terminal_colors.background.2,
            ),
            foreground: ExtRgb(
                terminal_colors.foreground.0,
                terminal_colors.foreground.1,
                terminal_colors.foreground.2,
            ),
            palette: terminal_colors
                .palette
                .map(|opt| opt.map(|c| ExtRgb(c.0, c.1, c.2))),
        };
        let resolved = spec.resolve(Some(&probe));
        let mut theme = generate_theme(terminal_colors);
        // Convert the extension's Rgb into phi-tui's and overlay via the
        // shared role table — `phi_tui::apply_palette` is the single place
        // that knows role names, shared with the inline session TUI.
        let palette: BTreeMap<String, Rgb> = resolved
            .iter()
            .map(|(name, c)| (name.clone(), ext_to_tui_rgb(*c)))
            .collect();
        phi_tui::apply_palette(&mut theme, &palette);
        Self::from_theme(&theme)
    }
}
