//! Built-in visual extension: registers Phi's default spinner,
//! thinking expressions, and a declarative default theme so that
//! the "out-of-the-box" look is itself an extension.
//!
//! When the mux TUI discovers extensions, this built-in is always
//! present. An extension that registers a theme or spinner with the
//! same name will be rejected (duplicate registration), so third-party
//! extensions should use distinct names. An extension that registers
//! a theme with a different name will appear alongside this one; the
//! mux uses the first registered theme (which is the built-in unless
//! a config extension is loaded first).

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::Result;
use phi_ext::{
    Extension, ExtensionHost, ExtensionManifest, Rgb, SpinnerSpec, TerminalColorsProbe,
    TextEffectSpec, ThemeSpec, ThinkingExpressionSpec,
};

/// The built-in visual extension.
///
/// Registers:
/// - `phi:default` spinner (braille dot frames, 80ms interval)
/// - `phi:default` thinking expressions (7 phrases, 2s interval)
/// - `phi:default` theme (a resolver that derives the full palette
///   from the terminal's actual OSC-reported colours at resolve time,
///   matching opencode's auto-theming approach)
/// - `phi:default` text effects: gradient, shimmer, and equalizer
///   (wrapping the corresponding `phi_tui::spinner` animation functions)
pub struct DefaultVisualExtension;

impl Extension for DefaultVisualExtension {
    fn manifest(&self) -> ExtensionManifest {
        ExtensionManifest {
            id: "phi:default-visual".to_string(),
            name: "Phi Default Visual".to_string(),
            version: "0.1.0".to_string(),
            description: "Built-in default spinner, thinking expressions, theme, and text effects"
                .to_string(),
        }
    }

    fn register(&self, host: &mut dyn ExtensionHost) -> Result<()> {
        // -- Default spinner (braille dots) --
        host.register_spinner(SpinnerSpec::new(
            "phi:default",
            vec![
                "\u{280B}".to_string(),
                "\u{2819}".to_string(),
                "\u{2839}".to_string(),
                "\u{2838}".to_string(),
                "\u{283C}".to_string(),
                "\u{2834}".to_string(),
                "\u{2826}".to_string(),
                "\u{2827}".to_string(),
                "\u{2807}".to_string(),
                "\u{280F}".to_string(),
            ],
            80,
        ))?;

        // -- Default thinking expressions --
        // One or two quiet words each: the gradient animation carries the
        // visual weight, the label is just a status word.
        host.register_thinking_expression(ThinkingExpressionSpec::new(
            "phi:default",
            vec![
                "thinking...".to_string(),
                "reasoning...".to_string(),
                "pondering...".to_string(),
                "working...".to_string(),
            ],
            "running...",
            2000,
        ))?;

        // -- Default theme (resolver-based, adapts to the terminal) --
        //
        // Unlike a static palette baked from `default_terminal_colors()`,
        // a resolver closure receives the *actual* terminal colours (OSC
        // 10/11/4) at resolve time and derives the full theme from them —
        // matching opencode's `generateSystem(colors, pick)` approach. This
        // ensures the gray ramp, surfaces, borders, and ANSI mapping all
        // reflect the user's real terminal rather than hardcoded defaults.
        //
        // When `from_extension_theme` resolves this spec, the resolver
        // returns a palette derived from the same probe that was used to
        // generate the base `Theme`, so the overlay is a no-op (same
        // values). Third-party extension themes with static palettes still
        // work — they only override the roles they specify.
        host.register_theme(ThemeSpec::with_resolver(
            "phi:default",
            Arc::new(|probe: Option<&TerminalColorsProbe>| {
                let colors = probe_to_terminal_colors(probe);
                let theme = phi_tui::generate_theme(&colors);
                theme_to_palette(&theme)
            }),
        ))?;

        // -- Default text effects (gradient, shimmer, equalizer) --
        //
        // Each effect wraps the corresponding `phi_tui::spinner` animation
        // function, converting `phi_ext::Rgb` <-> `phi_tui::palette::Rgb` at
        // the boundary. The handler signature is
        //   Fn(&str, [Rgb; 2], Rgb, u64) -> String
        // where the array is `[from, to]` gradient endpoints, the second `Rgb`
        // is the shimmer accent, and `u64` is elapsed milliseconds.

        // Gradient: per-character left-to-right colour blend. Ignores shimmer
        // and elapsed time.
        host.register_text_effect(TextEffectSpec::new(
            "phi:default:gradient",
            "gradient",
            Arc::new(
                |text: &str, endpoints: [Rgb; 2], _shimmer: Rgb, _elapsed_ms: u64| {
                    let from =
                        phi_tui::palette::Rgb(endpoints[0].0, endpoints[0].1, endpoints[0].2);
                    let to = phi_tui::palette::Rgb(endpoints[1].0, endpoints[1].1, endpoints[1].2);
                    phi_tui::gradient_text(text, from, to)
                },
            ),
        ))?;

        // Shimmer: percentage-based ANSI-aware sweep that blends each
        // character toward the shimmer accent colour. Ignores the gradient
        // endpoints; uses elapsed time for the sweep phase.
        host.register_text_effect(TextEffectSpec::new(
            "phi:default:shimmer",
            "shimmer",
            Arc::new(
                |text: &str, _endpoints: [Rgb; 2], shimmer: Rgb, elapsed_ms: u64| {
                    let shimmer = phi_tui::palette::Rgb(shimmer.0, shimmer.1, shimmer.2);
                    phi_tui::shimmer_ansi(text, shimmer, elapsed_ms)
                },
            ),
        ))?;

        // Equalizer: 5-bar VU-meter block-character cluster. Ignores all
        // colour arguments; uses elapsed time for the phase-shifted sine waves.
        host.register_text_effect(TextEffectSpec::new(
            "phi:default:equalizer",
            "equalizer",
            Arc::new(
                |_text: &str, _endpoints: [Rgb; 2], _shimmer: Rgb, elapsed_ms: u64| {
                    phi_tui::equalizer_glyphs(elapsed_ms)
                },
            ),
        ))?;

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Theme resolver helpers
// ---------------------------------------------------------------------------

/// Convert a `TerminalColorsProbe` (from `phi-ext`, which avoids a
/// cross-crate dependency on `phi-tui`) back into a `phi_tui::TerminalColors`
/// so the resolver can feed it into `generate_theme`.
///
/// When `probe` is `None` (no terminal probe available), falls back to the
/// standard ANSI defaults — the same behaviour as before this change, but
/// now only as a last resort rather than the always-on default.
fn probe_to_terminal_colors(probe: Option<&TerminalColorsProbe>) -> phi_tui::TerminalColors {
    match probe {
        Some(p) => phi_tui::TerminalColors {
            background: phi_tui::palette::Rgb(p.background.0, p.background.1, p.background.2),
            foreground: phi_tui::palette::Rgb(p.foreground.0, p.foreground.1, p.foreground.2),
            palette: p
                .palette
                .map(|opt| opt.map(|c| phi_tui::palette::Rgb(c.0, c.1, c.2))),
        },
        None => phi_tui::default_terminal_colors(),
    }
}

/// Serialize a `phi_tui::Theme` into the role-name → `Rgb` map that
/// `ThemeSpec::resolve` is expected to return.
///
/// Every role in the `Theme` struct is included so that `apply_extension_palette`
/// in the mux can overlay the complete palette. For the built-in default theme
/// this is a no-op overlay (same values as the base `generate_theme`), but the
/// full map is available for any consumer that wants to inspect or compare
/// individual roles.
fn theme_to_palette(theme: &phi_tui::Theme) -> BTreeMap<String, Rgb> {
    let mut p = BTreeMap::new();

    // Core
    p.insert(
        "foreground".into(),
        Rgb(theme.foreground.0, theme.foreground.1, theme.foreground.2),
    );
    p.insert(
        "background".into(),
        Rgb(theme.background.0, theme.background.1, theme.background.2),
    );
    p.insert(
        "muted".into(),
        Rgb(theme.muted.0, theme.muted.1, theme.muted.2),
    );
    p.insert(
        "heading".into(),
        Rgb(theme.heading.0, theme.heading.1, theme.heading.2),
    );

    // Surfaces
    p.insert(
        "surface".into(),
        Rgb(theme.surface.0, theme.surface.1, theme.surface.2),
    );
    p.insert(
        "surface_raised".into(),
        Rgb(
            theme.surface_raised.0,
            theme.surface_raised.1,
            theme.surface_raised.2,
        ),
    );
    p.insert(
        "border".into(),
        Rgb(theme.border.0, theme.border.1, theme.border.2),
    );

    // Accents
    p.insert(
        "accent".into(),
        Rgb(theme.accent.0, theme.accent.1, theme.accent.2),
    );
    p.insert(
        "accent_2".into(),
        Rgb(theme.accent_2.0, theme.accent_2.1, theme.accent_2.2),
    );

    // Status
    p.insert(
        "running".into(),
        Rgb(theme.running.0, theme.running.1, theme.running.2),
    );
    p.insert(
        "success".into(),
        Rgb(theme.success.0, theme.success.1, theme.success.2),
    );
    p.insert(
        "success_subtle".into(),
        Rgb(
            theme.success_subtle.0,
            theme.success_subtle.1,
            theme.success_subtle.2,
        ),
    );
    p.insert(
        "warning".into(),
        Rgb(theme.warning.0, theme.warning.1, theme.warning.2),
    );
    p.insert(
        "error".into(),
        Rgb(theme.error.0, theme.error.1, theme.error.2),
    );

    // Chrome
    p.insert(
        "statusbar_bg".into(),
        Rgb(
            theme.statusbar_bg.0,
            theme.statusbar_bg.1,
            theme.statusbar_bg.2,
        ),
    );

    // ANSI palette
    for i in 0..16u8 {
        let key = format!("ansi_{i}");
        p.insert(
            key,
            Rgb(
                theme.ansi[i as usize].0,
                theme.ansi[i as usize].1,
                theme.ansi[i as usize].2,
            ),
        );
    }

    // Chat / inline TUI roles
    p.insert(
        "tool_accent".into(),
        Rgb(
            theme.tool_accent.0,
            theme.tool_accent.1,
            theme.tool_accent.2,
        ),
    );
    p.insert(
        "tool_title".into(),
        Rgb(theme.tool_title.0, theme.tool_title.1, theme.tool_title.2),
    );
    p.insert(
        "tool_output".into(),
        Rgb(
            theme.tool_output.0,
            theme.tool_output.1,
            theme.tool_output.2,
        ),
    );
    p.insert(
        "subcall_accent".into(),
        Rgb(
            theme.subcall_accent.0,
            theme.subcall_accent.1,
            theme.subcall_accent.2,
        ),
    );
    p.insert("path".into(), Rgb(theme.path.0, theme.path.1, theme.path.2));
    p.insert(
        "string".into(),
        Rgb(theme.string.0, theme.string.1, theme.string.2),
    );
    p.insert(
        "number".into(),
        Rgb(theme.number.0, theme.number.1, theme.number.2),
    );
    p.insert(
        "user_fg".into(),
        Rgb(theme.user_fg.0, theme.user_fg.1, theme.user_fg.2),
    );
    p.insert(
        "assistant_fg".into(),
        Rgb(
            theme.assistant_fg.0,
            theme.assistant_fg.1,
            theme.assistant_fg.2,
        ),
    );
    p.insert(
        "panel_repl".into(),
        Rgb(theme.panel_repl.0, theme.panel_repl.1, theme.panel_repl.2),
    );

    // Chat backgrounds
    p.insert(
        "user_message_bg".into(),
        Rgb(
            theme.user_message_bg.0,
            theme.user_message_bg.1,
            theme.user_message_bg.2,
        ),
    );
    p.insert(
        "tool_pending_bg".into(),
        Rgb(
            theme.tool_pending_bg.0,
            theme.tool_pending_bg.1,
            theme.tool_pending_bg.2,
        ),
    );
    p.insert(
        "tool_success_bg".into(),
        Rgb(
            theme.tool_success_bg.0,
            theme.tool_success_bg.1,
            theme.tool_success_bg.2,
        ),
    );
    p.insert(
        "tool_error_bg".into(),
        Rgb(
            theme.tool_error_bg.0,
            theme.tool_error_bg.1,
            theme.tool_error_bg.2,
        ),
    );
    p.insert(
        "repl_bg".into(),
        Rgb(theme.repl_bg.0, theme.repl_bg.1, theme.repl_bg.2),
    );

    // Nested tool backgrounds
    p.insert(
        "tool_pending_bg_nested".into(),
        Rgb(
            theme.tool_pending_bg_nested.0,
            theme.tool_pending_bg_nested.1,
            theme.tool_pending_bg_nested.2,
        ),
    );
    p.insert(
        "tool_success_bg_nested".into(),
        Rgb(
            theme.tool_success_bg_nested.0,
            theme.tool_success_bg_nested.1,
            theme.tool_success_bg_nested.2,
        ),
    );
    p.insert(
        "tool_error_bg_nested".into(),
        Rgb(
            theme.tool_error_bg_nested.0,
            theme.tool_error_bg_nested.1,
            theme.tool_error_bg_nested.2,
        ),
    );

    // Chrome roles
    p.insert(
        "chrome_agent".into(),
        Rgb(
            theme.chrome_agent.0,
            theme.chrome_agent.1,
            theme.chrome_agent.2,
        ),
    );
    p.insert(
        "chrome_cost".into(),
        Rgb(
            theme.chrome_cost.0,
            theme.chrome_cost.1,
            theme.chrome_cost.2,
        ),
    );
    p.insert(
        "chrome_dir".into(),
        Rgb(theme.chrome_dir.0, theme.chrome_dir.1, theme.chrome_dir.2),
    );
    p.insert(
        "chrome_model".into(),
        Rgb(
            theme.chrome_model.0,
            theme.chrome_model.1,
            theme.chrome_model.2,
        ),
    );

    // Thinking levels
    p.insert(
        "thinking_off".into(),
        Rgb(
            theme.thinking_off.0,
            theme.thinking_off.1,
            theme.thinking_off.2,
        ),
    );
    p.insert(
        "thinking_minimal".into(),
        Rgb(
            theme.thinking_minimal.0,
            theme.thinking_minimal.1,
            theme.thinking_minimal.2,
        ),
    );
    p.insert(
        "thinking_low".into(),
        Rgb(
            theme.thinking_low.0,
            theme.thinking_low.1,
            theme.thinking_low.2,
        ),
    );
    p.insert(
        "thinking_medium".into(),
        Rgb(
            theme.thinking_medium.0,
            theme.thinking_medium.1,
            theme.thinking_medium.2,
        ),
    );
    p.insert(
        "thinking_high".into(),
        Rgb(
            theme.thinking_high.0,
            theme.thinking_high.1,
            theme.thinking_high.2,
        ),
    );
    p.insert(
        "thinking_xhigh".into(),
        Rgb(
            theme.thinking_xhigh.0,
            theme.thinking_xhigh.1,
            theme.thinking_xhigh.2,
        ),
    );

    // Markdown
    p.insert(
        "md_repl_fence".into(),
        Rgb(
            theme.md_repl_fence.0,
            theme.md_repl_fence.1,
            theme.md_repl_fence.2,
        ),
    );
    p.insert(
        "md_marker_accent".into(),
        Rgb(
            theme.md_marker_accent.0,
            theme.md_marker_accent.1,
            theme.md_marker_accent.2,
        ),
    );
    p.insert(
        "md_task_done".into(),
        Rgb(
            theme.md_task_done.0,
            theme.md_task_done.1,
            theme.md_task_done.2,
        ),
    );
    p.insert(
        "md_quote_bar".into(),
        Rgb(
            theme.md_quote_bar.0,
            theme.md_quote_bar.1,
            theme.md_quote_bar.2,
        ),
    );
    p.insert(
        "md_subhead".into(),
        Rgb(theme.md_subhead.0, theme.md_subhead.1, theme.md_subhead.2),
    );
    p.insert(
        "md_out_base".into(),
        Rgb(
            theme.md_out_base.0,
            theme.md_out_base.1,
            theme.md_out_base.2,
        ),
    );
    p.insert(
        "md_out_err".into(),
        Rgb(theme.md_out_err.0, theme.md_out_err.1, theme.md_out_err.2),
    );
    p.insert(
        "md_out_warn".into(),
        Rgb(
            theme.md_out_warn.0,
            theme.md_out_warn.1,
            theme.md_out_warn.2,
        ),
    );
    p.insert(
        "md_out_ok".into(),
        Rgb(theme.md_out_ok.0, theme.md_out_ok.1, theme.md_out_ok.2),
    );
    p.insert(
        "md_out_path".into(),
        Rgb(
            theme.md_out_path.0,
            theme.md_out_path.1,
            theme.md_out_path.2,
        ),
    );
    p.insert(
        "md_out_num".into(),
        Rgb(theme.md_out_num.0, theme.md_out_num.1, theme.md_out_num.2),
    );
    p.insert(
        "md_out_str".into(),
        Rgb(theme.md_out_str.0, theme.md_out_str.1, theme.md_out_str.2),
    );
    p.insert(
        "md_out_url".into(),
        Rgb(theme.md_out_url.0, theme.md_out_url.1, theme.md_out_url.2),
    );
    p.insert(
        "md_diff_add".into(),
        Rgb(
            theme.md_diff_add.0,
            theme.md_diff_add.1,
            theme.md_diff_add.2,
        ),
    );
    p.insert(
        "md_diff_del".into(),
        Rgb(
            theme.md_diff_del.0,
            theme.md_diff_del.1,
            theme.md_diff_del.2,
        ),
    );

    // Syntax
    p.insert(
        "syntax_comment".into(),
        Rgb(
            theme.syntax_comment.0,
            theme.syntax_comment.1,
            theme.syntax_comment.2,
        ),
    );
    p.insert(
        "syntax_keyword".into(),
        Rgb(
            theme.syntax_keyword.0,
            theme.syntax_keyword.1,
            theme.syntax_keyword.2,
        ),
    );
    p.insert(
        "syntax_string".into(),
        Rgb(
            theme.syntax_string.0,
            theme.syntax_string.1,
            theme.syntax_string.2,
        ),
    );
    p.insert(
        "syntax_number".into(),
        Rgb(
            theme.syntax_number.0,
            theme.syntax_number.1,
            theme.syntax_number.2,
        ),
    );
    p.insert(
        "syntax_function".into(),
        Rgb(
            theme.syntax_function.0,
            theme.syntax_function.1,
            theme.syntax_function.2,
        ),
    );
    p.insert(
        "syntax_type".into(),
        Rgb(
            theme.syntax_type.0,
            theme.syntax_type.1,
            theme.syntax_type.2,
        ),
    );
    p.insert(
        "syntax_operator".into(),
        Rgb(
            theme.syntax_operator.0,
            theme.syntax_operator.1,
            theme.syntax_operator.2,
        ),
    );
    p.insert(
        "syntax_variable".into(),
        Rgb(
            theme.syntax_variable.0,
            theme.syntax_variable.1,
            theme.syntax_variable.2,
        ),
    );
    p.insert(
        "syntax_builtin".into(),
        Rgb(
            theme.syntax_builtin.0,
            theme.syntax_builtin.1,
            theme.syntax_builtin.2,
        ),
    );

    // CLI glyphs
    p.insert(
        "glyph_active".into(),
        Rgb(
            theme.glyph_active.0,
            theme.glyph_active.1,
            theme.glyph_active.2,
        ),
    );
    p.insert(
        "glyph_ok".into(),
        Rgb(theme.glyph_ok.0, theme.glyph_ok.1, theme.glyph_ok.2),
    );
    p.insert(
        "glyph_err".into(),
        Rgb(theme.glyph_err.0, theme.glyph_err.1, theme.glyph_err.2),
    );

    p
}

#[cfg(test)]
mod tests {
    use phi_ext::{Rgb, RuntimeBuilder, TerminalColorsProbe};

    use super::DefaultVisualExtension;

    #[test]
    fn default_visual_extension_registers_spinner() {
        let rt = RuntimeBuilder::new()
            .register_extension(DefaultVisualExtension)
            .build()
            .unwrap();

        let spinner = rt.first_spinner().unwrap();
        assert_eq!(spinner.name, "phi:default");
        assert_eq!(spinner.frames.len(), 10);
        assert_eq!(spinner.frame_interval_ms, 80);
        assert_eq!(spinner.frame_at(0), "\u{280B}");
    }

    #[test]
    fn default_visual_extension_registers_thinking_expressions() {
        let rt = RuntimeBuilder::new()
            .register_extension(DefaultVisualExtension)
            .build()
            .unwrap();

        let expr = rt.first_thinking_expression().unwrap();
        assert_eq!(expr.name, "phi:default");
        assert_eq!(expr.expressions.len(), 4);
        assert_eq!(expr.tool_expression, "running...");
        assert_eq!(expr.expression_interval_ms, 2000);
        assert_eq!(expr.expression(0, false), "thinking...");
        assert_eq!(expr.expression(0, true), "running...");
    }

    #[test]
    fn default_visual_extension_registers_theme() {
        let rt = RuntimeBuilder::new()
            .register_extension(DefaultVisualExtension)
            .build()
            .unwrap();

        let theme = rt.first_theme().unwrap();
        assert_eq!(theme.name, "phi:default");
        // The built-in default theme uses a resolver (not a static palette),
        // so the static `palette` field is empty. The palette is produced
        // by resolving the spec with a terminal colour probe.
        assert!(theme.palette.is_empty());
        assert!(theme.resolver.is_some());

        // Resolve with default terminal colours and verify the full palette.
        let probe = TerminalColorsProbe {
            background: Rgb(0, 0, 0),
            foreground: Rgb(192, 192, 192),
            palette: [None; 16],
        };
        let resolved = theme.resolve(Some(&probe));
        assert!(resolved.contains_key("foreground"));
        assert!(resolved.contains_key("background"));
        assert!(resolved.contains_key("accent"));
        assert!(resolved.contains_key("border"));
        // ANSI palette should have all 16 entries
        for i in 0..16u8 {
            let key = format!("ansi_{i}");
            assert!(resolved.contains_key(&key), "missing {key}");
        }
    }

    #[test]
    fn default_theme_resolver_adapts_to_terminal_colors() {
        // The resolver must derive different themes from different terminal
        // backgrounds — this is the core of opencode-style auto-theming.
        // A pure-black background should produce darker surfaces than a
        // lighter dark background (e.g. #1e1e2e).
        let rt = RuntimeBuilder::new()
            .register_extension(DefaultVisualExtension)
            .build()
            .unwrap();

        let theme = rt.first_theme().unwrap();

        let black_bg = TerminalColorsProbe {
            background: Rgb(0, 0, 0),
            foreground: Rgb(192, 192, 192),
            palette: [None; 16],
        };
        let dark_bg = TerminalColorsProbe {
            background: Rgb(0x1e, 0x1e, 0x2e),
            foreground: Rgb(0xc0, 0xc0, 0xc0),
            palette: [None; 16],
        };

        let black_palette = theme.resolve(Some(&black_bg));
        let dark_palette = theme.resolve(Some(&dark_bg));

        // Surfaces should differ — the gray ramp is derived from the
        // background, so a non-black bg produces lighter surfaces.
        let black_surface = black_palette.get("surface").unwrap();
        let dark_surface = dark_palette.get("surface").unwrap();
        assert_ne!(
            black_surface, dark_surface,
            "surface should differ for different terminal backgrounds"
        );

        // Borders should also differ.
        let black_border = black_palette.get("border").unwrap();
        let dark_border = dark_palette.get("border").unwrap();
        assert_ne!(
            black_border, dark_border,
            "border should differ for different terminal backgrounds"
        );

        // Background should match the probe.
        assert_eq!(black_palette.get("background").unwrap(), &Rgb(0, 0, 0));
        assert_eq!(
            dark_palette.get("background").unwrap(),
            &Rgb(0x1e, 0x1e, 0x2e)
        );
    }

    #[test]
    fn default_visual_extension_registers_text_effects() {
        phi_tui::color_cap::force_color_capability(phi_tui::color_cap::ColorCapability::TrueColor);
        let rt = RuntimeBuilder::new()
            .register_extension(DefaultVisualExtension)
            .build()
            .unwrap();

        // All three text effects should be registered under "phi:default".
        assert_eq!(
            rt.list_text_effects(),
            vec![
                "phi:default:equalizer",
                "phi:default:gradient",
                "phi:default:shimmer"
            ]
        );

        // Gradient: applies a per-character colour blend.
        let gradient = rt.get_text_effect("phi:default:gradient").unwrap();
        let gradient_result =
            gradient.apply("hello", Rgb(0, 0, 0), Rgb(255, 255, 255), Rgb(0, 255, 0), 0);
        assert!(gradient_result.contains("[38;2;"));
        assert!(gradient_result.ends_with("[0m"));

        // Shimmer: should preserve visible text characters.
        let shimmer = rt.get_text_effect("phi:default:shimmer").unwrap();
        let shimmer_result = shimmer.apply(
            "read",
            Rgb(130, 180, 230),
            Rgb(200, 100, 50),
            Rgb(80, 250, 120),
            500,
        );
        // Shimmer wraps each char in ANSI codes, so check for
        // individual characters rather than the bare substring.
        assert!(shimmer_result.contains('r'));
        assert!(shimmer_result.contains('e'));
        assert!(shimmer_result.contains('a'));
        assert!(shimmer_result.contains('d'));

        // Equalizer: should produce 5 block characters.
        let equalizer = rt.get_text_effect("phi:default:equalizer").unwrap();
        let equalizer_result = equalizer.apply("", Rgb(0, 0, 0), Rgb(0, 0, 0), Rgb(0, 0, 0), 42);
        assert_eq!(equalizer_result.chars().count(), 5);
        for c in equalizer_result.chars() {
            assert!(
                phi_tui::EQUALIZER_BARS.contains(&c.to_string().as_str()),
                "unexpected equalizer char: {c}"
            );
        }
    }

    #[test]
    fn runtime_lists_all_visual_specs() {
        let rt = RuntimeBuilder::new()
            .register_extension(DefaultVisualExtension)
            .build()
            .unwrap();

        assert_eq!(rt.list_themes(), vec!["phi:default"]);
        assert_eq!(rt.list_spinners(), vec!["phi:default"]);
        assert_eq!(rt.list_thinking_expressions(), vec!["phi:default"]);
        assert_eq!(
            rt.list_text_effects(),
            vec![
                "phi:default:equalizer",
                "phi:default:gradient",
                "phi:default:shimmer"
            ]
        );
    }
}
