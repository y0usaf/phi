//! Style packs (WS10): a named bundle of visual registrations activated
//! as a unit, so `/style <name>` can swap Phi's entire visual identity at
//! runtime.
//!
//! A style's members reuse the *existing* registration shapes — a
//! [`ThemeSpec`], a [`SpinnerSpec`], a [`ThinkingExpressionSpec`], and a
//! set of [`ToolRenderSpec`]s — rather than inventing a parallel
//! vocabulary (one declaration mechanism). Members are bundled inline, not
//! referenced by name, so an inactive pack's theme never leaks into the
//! base `first_theme()` fallback order.
//!
//! Exactly one style is active at a time ([`crate::AppRuntime::set_active_style`]).
//! Every member is optional: a member left empty falls through to the base
//! registrations, so the shipped default style is simply the empty bundle
//! named `phi` and a builtins-less build degrades the same way it does for
//! themes today.

use crate::{ChromeSpec, SpinnerSpec, ThemeSpec, ThinkingExpressionSpec, ToolRenderSpec};

/// A named visual style bundle registered by an extension.
#[derive(Clone, Default)]
pub struct StyleSpec {
    pub name: String,
    /// Palette used while this style is active (falls back to the first
    /// registered theme when absent).
    pub theme: Option<ThemeSpec>,
    /// Spinner used while this style is active.
    pub spinner: Option<SpinnerSpec>,
    /// Thinking expressions used while this style is active.
    pub thinking_expression: Option<ThinkingExpressionSpec>,
    /// Tool render specs overlaid on the base registry while this style is
    /// active (keyed by tool name; a style spec wins over the base spec for
    /// the same tool).
    pub tool_renderers: Vec<ToolRenderSpec>,
    /// Transcript tool chrome (anchor composition, gutter glyphs, result
    /// prefix, hidden-lines marker, card-vs-flat container) used while this
    /// style is active (falls back to [`ChromeSpec::default`] — Phi's stock
    /// look — when absent).
    pub chrome: Option<ChromeSpec>,
}

impl StyleSpec {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
        }
    }

    pub fn with_theme(mut self, theme: ThemeSpec) -> Self {
        self.theme = Some(theme);
        self
    }

    pub fn with_spinner(mut self, spinner: SpinnerSpec) -> Self {
        self.spinner = Some(spinner);
        self
    }

    pub fn with_thinking_expression(mut self, expr: ThinkingExpressionSpec) -> Self {
        self.thinking_expression = Some(expr);
        self
    }

    pub fn with_tool_renderers(mut self, specs: Vec<ToolRenderSpec>) -> Self {
        self.tool_renderers = specs;
        self
    }

    pub fn with_chrome(mut self, chrome: ChromeSpec) -> Self {
        self.chrome = Some(chrome);
        self
    }
}
