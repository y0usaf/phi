use anyhow::Result;

use crate::{
    CommandSpec, EventHandlerRegistration, ExtensionManifest, FlagSpec, MessageRendererSpec,
    ProviderConfig, ShortcutSpec, ToolSpec,
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

    // ── Provider registration ───────────────────────────────────────────────
    fn register_provider(&mut self, config: ProviderConfig) -> Result<()>;
    fn unregister_provider(&mut self, name: &str) -> Result<()>;
}
