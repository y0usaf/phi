//! Visual context: extension-registered visual specs passed through the
//! rendering pipeline so the TUI can use extension-provided themes,
//! spinners, thinking expressions, and text effects.
//!
//! When the visual runtime has registered specs, they are resolved into
//! this context at draw time and threaded through `render_scene` →
//! `render_sidebar` → `render_statusbar`. When no extension has
//! registered a spec, the fields are `None` and the rendering code
//! falls back to the built-in defaults.

use phi_ext::{SpinnerSpec, TextEffectSpec, ThinkingExpressionSpec};

/// Extension-provided visual specs for a single draw frame.
///
/// All fields are optional: an extension may register only a spinner, only
/// a theme, or all visual specs. The rendering code checks each field and
/// falls back to the built-in default when it is `None`.
#[derive(Clone, Default)]
pub struct VisualContext {
    /// Extension-registered spinner (frame array + interval).
    pub spinner: Option<SpinnerSpec>,
    /// Extension-registered thinking expressions.
    pub thinking_expression: Option<ThinkingExpressionSpec>,
    /// Extension-registered text effects, keyed by kind.
    pub text_effects: Vec<TextEffectSpec>,
}

impl VisualContext {
    /// Build a `VisualContext` from an extension runtime.
    ///
    /// Takes the first registered spec of each kind. Extensions are
    /// registered in order, so the first one is the "default" extension
    /// visual.
    pub fn from_runtime(runtime: Option<&phi_ext::AppRuntime>) -> Self {
        let runtime = match runtime {
            Some(rt) => rt,
            None => return Self::default(),
        };
        Self {
            spinner: runtime.first_spinner().cloned(),
            thinking_expression: runtime.first_thinking_expression().cloned(),
            text_effects: runtime
                .list_text_effects()
                .iter()
                .filter_map(|name| runtime.get_text_effect(name).cloned())
                .collect(),
        }
    }

    /// Resolve the spinner frame for a given millisecond timestamp.
    ///
    /// Returns the extension spinner frame if a spinner is registered,
    /// otherwise falls back to the built-in braille spinner.
    pub fn spinner_frame(&self, now_ms: u64) -> String {
        if let Some(spinner) = &self.spinner {
            spinner.frame_at(now_ms).to_string()
        } else {
            phi_tui::spinner_frame_at(now_ms).to_string()
        }
    }

    /// Resolve the thinking expression for a given elapsed time.
    ///
    /// Returns the extension expression if registered, otherwise falls
    /// back to the built-in thinking expressions.
    pub fn thinking_expression(&self, elapsed_ms: u64, tool_active: bool) -> String {
        if let Some(expr) = &self.thinking_expression {
            expr.expression(elapsed_ms, tool_active).to_string()
        } else {
            phi_tui::thinking_expression(elapsed_ms, tool_active).to_string()
        }
    }

    /// Apply a text effect by kind, if one is registered.
    ///
    /// Returns `Some(styled_string)` if an effect with the given kind is
    /// registered, or `None` if no matching effect exists (the caller
    /// should fall back to the built-in rendering).
    ///
    /// The `from`/`to` parameters are the gradient endpoints, `shimmer`
    /// is the accent colour for shimmer effects, and `elapsed_ms` is the
    /// wall-clock elapsed time for animations.
    #[allow(dead_code)]
    pub fn apply_text_effect(
        &self,
        kind: &str,
        text: &str,
        from: phi_ext::Rgb,
        to: phi_ext::Rgb,
        shimmer: phi_ext::Rgb,
        elapsed_ms: u64,
    ) -> Option<String> {
        self.text_effects
            .iter()
            .find(|e| e.kind == kind)
            .map(|e| e.apply(text, from, to, shimmer, elapsed_ms))
    }
}
