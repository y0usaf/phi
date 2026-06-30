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
        apply_extension_palette(&mut theme, &resolved);
        Self::from_theme(&theme)
    }
}

/// Overlay an extension-provided palette map onto a `Theme`.
///
/// For each role name in the map, convert the `ExtRgb` to `Rgb` and write
/// it into the corresponding `Theme` field. Unknown role names are silently
/// ignored — an extension only needs to provide the colours it cares about.
fn apply_extension_palette(theme: &mut Theme, palette: &BTreeMap<String, ExtRgb>) {
    // Helper: look up a role and convert.
    macro_rules! role {
        ($field:ident, $name:literal) => {
            if let Some(c) = palette.get($name) {
                theme.$field = ext_to_tui_rgb(*c);
            }
        };
    }

    // Core
    role!(foreground, "foreground");
    role!(background, "background");
    role!(muted, "muted");
    role!(heading, "heading");

    // Surfaces
    role!(surface, "surface");
    role!(surface_raised, "surface_raised");
    role!(border, "border");

    // Accents
    role!(accent, "accent");
    role!(accent_2, "accent_2");

    // Status
    role!(running, "running");
    role!(success, "success");
    role!(success_subtle, "success_subtle");
    role!(warning, "warning");
    role!(error, "error");

    // Chrome
    role!(statusbar_bg, "statusbar_bg");

    // ANSI palette (ansi_0 through ansi_15)
    for i in 0..16u8 {
        let key = format!("ansi_{i}");
        if let Some(c) = palette.get(&key) {
            theme.ansi[i as usize] = ext_to_tui_rgb(*c);
        }
    }

    // Chat / inline TUI
    role!(tool_accent, "tool_accent");
    role!(tool_title, "tool_title");
    role!(tool_output, "tool_output");
    role!(subcall_accent, "subcall_accent");
    role!(path, "path");
    role!(string, "string");
    role!(number, "number");
    role!(user_fg, "user_fg");
    role!(assistant_fg, "assistant_fg");
    role!(panel_repl, "panel_repl");

    // Chat backgrounds
    role!(user_message_bg, "user_message_bg");
    role!(tool_pending_bg, "tool_pending_bg");
    role!(tool_success_bg, "tool_success_bg");
    role!(tool_error_bg, "tool_error_bg");
    role!(repl_bg, "repl_bg");

    // Nested tool backgrounds
    role!(tool_pending_bg_nested, "tool_pending_bg_nested");
    role!(tool_success_bg_nested, "tool_success_bg_nested");
    role!(tool_error_bg_nested, "tool_error_bg_nested");

    // Chrome roles
    role!(chrome_agent, "chrome_agent");
    role!(chrome_cost, "chrome_cost");
    role!(chrome_dir, "chrome_dir");
    role!(chrome_model, "chrome_model");

    // Thinking levels
    role!(thinking_off, "thinking_off");
    role!(thinking_minimal, "thinking_minimal");
    role!(thinking_low, "thinking_low");
    role!(thinking_medium, "thinking_medium");
    role!(thinking_high, "thinking_high");
    role!(thinking_xhigh, "thinking_xhigh");

    // Markdown
    role!(md_repl_fence, "md_repl_fence");
    role!(md_marker_accent, "md_marker_accent");
    role!(md_task_done, "md_task_done");
    role!(md_quote_bar, "md_quote_bar");
    role!(md_subhead, "md_subhead");
    role!(md_out_base, "md_out_base");
    role!(md_out_err, "md_out_err");
    role!(md_out_warn, "md_out_warn");
    role!(md_out_ok, "md_out_ok");
    role!(md_out_path, "md_out_path");
    role!(md_out_num, "md_out_num");
    role!(md_out_str, "md_out_str");
    role!(md_out_url, "md_out_url");
    role!(md_diff_add, "md_diff_add");
    role!(md_diff_del, "md_diff_del");

    // Syntax
    role!(syntax_comment, "syntax_comment");
    role!(syntax_keyword, "syntax_keyword");
    role!(syntax_string, "syntax_string");
    role!(syntax_number, "syntax_number");
    role!(syntax_function, "syntax_function");
    role!(syntax_type, "syntax_type");
    role!(syntax_operator, "syntax_operator");
    role!(syntax_variable, "syntax_variable");
    role!(syntax_builtin, "syntax_builtin");

    // CLI glyphs
    role!(glyph_active, "glyph_active");
    role!(glyph_ok, "glyph_ok");
    role!(glyph_err, "glyph_err");
}
