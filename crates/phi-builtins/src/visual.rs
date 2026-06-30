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
    Extension, ExtensionHost, ExtensionManifest, Rgb, SpinnerSpec, TextEffectSpec, ThemeSpec,
    ThinkingExpressionSpec,
};

/// The built-in visual extension.
///
/// Registers:
/// - `phi:default` spinner (braille dot frames, 80ms interval)
/// - `phi:default` thinking expressions (7 phrases, 2s interval)
/// - `phi:default` theme (a declarative palette derived from the
///   standard ANSI defaults, matching `phi_tui::default_terminal_colors`)
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
        host.register_thinking_expression(ThinkingExpressionSpec::new(
            "phi:default",
            vec![
                "thinking hard...".to_string(),
                "pondering...".to_string(),
                "cranking its neurons...".to_string(),
                "cooking up an answer...".to_string(),
                "crunching numbers...".to_string(),
                "stirring its circuits...".to_string(),
                "shuffling tokens...".to_string(),
            ],
            "doing something complex...",
            2000,
        ))?;

        // -- Default theme (declarative, from standard ANSI defaults) --
        let colors = phi_tui::default_terminal_colors();
        let theme = phi_tui::generate_theme(&colors);
        let mut palette = BTreeMap::new();

        // Core
        palette.insert(
            "foreground".into(),
            Rgb(theme.foreground.0, theme.foreground.1, theme.foreground.2),
        );
        palette.insert(
            "background".into(),
            Rgb(theme.background.0, theme.background.1, theme.background.2),
        );
        palette.insert(
            "muted".into(),
            Rgb(theme.muted.0, theme.muted.1, theme.muted.2),
        );
        palette.insert(
            "heading".into(),
            Rgb(theme.heading.0, theme.heading.1, theme.heading.2),
        );

        // Surfaces
        palette.insert(
            "surface".into(),
            Rgb(theme.surface.0, theme.surface.1, theme.surface.2),
        );
        palette.insert(
            "surface_raised".into(),
            Rgb(
                theme.surface_raised.0,
                theme.surface_raised.1,
                theme.surface_raised.2,
            ),
        );
        palette.insert(
            "border".into(),
            Rgb(theme.border.0, theme.border.1, theme.border.2),
        );

        // Accents
        palette.insert(
            "accent".into(),
            Rgb(theme.accent.0, theme.accent.1, theme.accent.2),
        );
        palette.insert(
            "accent_2".into(),
            Rgb(theme.accent_2.0, theme.accent_2.1, theme.accent_2.2),
        );

        // Status
        palette.insert(
            "running".into(),
            Rgb(theme.running.0, theme.running.1, theme.running.2),
        );
        palette.insert(
            "success".into(),
            Rgb(theme.success.0, theme.success.1, theme.success.2),
        );
        palette.insert(
            "warning".into(),
            Rgb(theme.warning.0, theme.warning.1, theme.warning.2),
        );
        palette.insert(
            "error".into(),
            Rgb(theme.error.0, theme.error.1, theme.error.2),
        );

        // Chrome
        palette.insert(
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
            palette.insert(
                key,
                Rgb(
                    theme.ansi[i as usize].0,
                    theme.ansi[i as usize].1,
                    theme.ansi[i as usize].2,
                ),
            );
        }

        host.register_theme(ThemeSpec::new("phi:default", palette))?;

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

#[cfg(test)]
mod tests {
    use phi_ext::{Rgb, RuntimeBuilder};

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
        assert_eq!(expr.expressions.len(), 7);
        assert_eq!(expr.tool_expression, "doing something complex...");
        assert_eq!(expr.expression_interval_ms, 2000);
        assert_eq!(expr.expression(0, false), "thinking hard...");
        assert_eq!(expr.expression(0, true), "doing something complex...");
    }

    #[test]
    fn default_visual_extension_registers_theme() {
        let rt = RuntimeBuilder::new()
            .register_extension(DefaultVisualExtension)
            .build()
            .unwrap();

        let theme = rt.first_theme().unwrap();
        assert_eq!(theme.name, "phi:default");
        assert!(theme.palette.contains_key("foreground"));
        assert!(theme.palette.contains_key("background"));
        assert!(theme.palette.contains_key("accent"));
        assert!(theme.palette.contains_key("border"));
        // ANSI palette should have all 16 entries
        for i in 0..16u8 {
            let key = format!("ansi_{i}");
            assert!(theme.palette.contains_key(&key), "missing {key}");
        }
    }

    #[test]
    fn default_visual_extension_registers_text_effects() {
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
