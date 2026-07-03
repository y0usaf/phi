use anyhow::Result;

use crate::{
    CommandSpec, EventHandlerRegistration, ExtensionManifest, FlagSpec, MessageRendererSpec,
    OverlaySpec, ProviderConfig, ShortcutSpec, ThemeSpec, ToolRenderSpec, ToolSpec,
};

pub trait Extension: Send + Sync {
    fn manifest(&self) -> ExtensionManifest;
    fn register(&self, host: &mut dyn ExtensionHost) -> Result<()>;
}

/// The registration surface available to extensions during `register()`.
///
/// This mirrors pi's `ExtensionAPI`: each method lets an extension add a
/// capability (tool, command, shortcut, flag, event subscription, etc.) to
/// the runtime.
///
/// Visual registrations (themes, spinners, thinking expressions, text
/// effects) allow extensions to provide the TUI's look-and-feel so the
/// core remains minimalist.
pub trait ExtensionHost {
    // ── Tool / Command / Shortcut / Flag ───────────────────────────────────
    fn register_tool(&mut self, tool: ToolSpec) -> Result<()>;
    fn register_command(&mut self, command: CommandSpec) -> Result<()>;
    fn register_shortcut(&mut self, shortcut: ShortcutSpec) -> Result<()>;
    fn register_flag(&mut self, flag: FlagSpec) -> Result<()>;

    // ── Event subscription ──────────────────────────────────────────────────
    fn subscribe(&mut self, handler: EventHandlerRegistration) -> Result<()>;

    // ── Message rendering ───────────────────────────────────────────────────
    fn register_message_renderer(&mut self, renderer: MessageRendererSpec) -> Result<()>;

    /// Register declarative display metadata for a tool (anchor verb,
    /// canonical args, body kind, summary category). Keyed by tool name,
    /// independent of `register_tool` — prelude-implemented tools have
    /// display metadata without being registry tools. Last write wins, so
    /// a user extension can restyle a builtin.
    fn register_tool_renderer(&mut self, spec: ToolRenderSpec) -> Result<()>;

    // ── Provider registration ───────────────────────────────────────────────
    fn register_provider(&mut self, config: ProviderConfig) -> Result<()>;
    fn unregister_provider(&mut self, name: &str) -> Result<()>;

    // ── Visual: themes ─────────────────────────────────────────────────────
    /// Register a named colour theme. Extensions can provide complete
    /// palettes (declarative) or resolvers that derive colours from the
    /// terminal's reported colours (programmatic).
    fn register_theme(&mut self, theme: ThemeSpec) -> Result<()>;

    // ── Visual: animations ───────────────────────────────────────────────
    /// Register a named spinner (frame array + interval).
    fn register_spinner(&mut self, spinner: crate::SpinnerSpec) -> Result<()>;

    /// Register named thinking expressions cycled while the agent works.
    fn register_thinking_expression(&mut self, expr: crate::ThinkingExpressionSpec) -> Result<()>;

    /// Register a named text effect (gradient, shimmer, etc.).
    fn register_text_effect(&mut self, effect: crate::TextEffectSpec) -> Result<()>;

    // ── UI: overlays ─────────────────────────────────────────────────────
    /// Register a named overlay (modal panel). Extensions can provide
    /// custom overlays that the TUI renders on top of the normal chrome.
    /// The overlay's render closure draws into a `DrawContext`, and its
    /// key-handler closure processes key events and returns an outcome.
    fn register_overlay(&mut self, overlay: OverlaySpec) -> Result<()>;

    // ── Visual: animations (convenience) ───────────────────────────────
    /// Register all animation specs at once.
    fn register_animation(&mut self, spec: AnimationSpec) -> Result<()> {
        match spec {
            AnimationSpec::Spinner(s) => self.register_spinner(s),
            AnimationSpec::ThinkingExpression(t) => self.register_thinking_expression(t),
            AnimationSpec::TextEffect(e) => self.register_text_effect(e),
        }
    }
}

/// A tagged animation spec for the convenience `register_animation` method.
#[derive(Clone)]
pub enum AnimationSpec {
    Spinner(crate::SpinnerSpec),
    ThinkingExpression(crate::ThinkingExpressionSpec),
    TextEffect(crate::TextEffectSpec),
}
