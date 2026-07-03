use std::collections::BTreeMap;

use crate::{
    CommandContext, CommandSpec, EventHandlerRegistration, ExtensionHost, FlagSpec,
    MessageRendererSpec, OverlaySpec, ProviderConfig, ShortcutSpec, SpinnerSpec, TextEffectSpec,
    ThemeSpec, ThinkingExpressionSpec, ToolSpec,
};
use anyhow::{Result, bail};

#[derive(Default)]
pub(crate) struct RegistryHost {
    pub(crate) tools: BTreeMap<String, ToolSpec>,
    pub(crate) commands: BTreeMap<(CommandContext, String), CommandSpec>,
    pub(crate) shortcuts: Vec<ShortcutSpec>,
    pub(crate) flags: BTreeMap<String, FlagSpec>,
    pub(crate) event_handlers: Vec<EventHandlerRegistration>,
    pub(crate) message_renderers: BTreeMap<String, MessageRendererSpec>,
    pub(crate) providers: BTreeMap<String, ProviderConfig>,
    // ── Visual ──────────────────────────────────────────────────────────
    pub(crate) themes: BTreeMap<String, ThemeSpec>,
    pub(crate) spinners: BTreeMap<String, SpinnerSpec>,
    pub(crate) thinking_expressions: BTreeMap<String, ThinkingExpressionSpec>,
    pub(crate) text_effects: BTreeMap<String, TextEffectSpec>,
    pub(crate) overlays: BTreeMap<String, OverlaySpec>,
}

impl ExtensionHost for RegistryHost {
    fn register_tool(&mut self, tool: ToolSpec) -> Result<()> {
        use std::collections::btree_map::Entry;
        let name = tool.name.clone();
        match self.tools.entry(name) {
            Entry::Vacant(v) => {
                v.insert(tool);
                Ok(())
            }
            Entry::Occupied(_) => bail!("tool '{}' is already registered", tool.name),
        }
    }

    fn register_command(&mut self, command: CommandSpec) -> Result<()> {
        use std::collections::btree_map::Entry;
        // Keyed by (context, name): the same name may register in different
        // contexts (session `/quit` vs mux `:quit`) without colliding.
        let key = (command.context, command.name.clone());
        match self.commands.entry(key) {
            Entry::Vacant(v) => {
                v.insert(command);
                Ok(())
            }
            Entry::Occupied(_) => bail!("command '{}' is already registered", command.name),
        }
    }

    fn register_shortcut(&mut self, shortcut: ShortcutSpec) -> Result<()> {
        self.shortcuts.push(shortcut);
        Ok(())
    }

    fn register_flag(&mut self, flag: FlagSpec) -> Result<()> {
        let name = flag.name.clone();
        if self.flags.contains_key(&name) {
            bail!("flag '{}' is already registered", name);
        }
        self.flags.insert(name, flag);
        Ok(())
    }

    fn subscribe(&mut self, handler: EventHandlerRegistration) -> Result<()> {
        self.event_handlers.push(handler);
        Ok(())
    }

    fn register_message_renderer(&mut self, renderer: MessageRendererSpec) -> Result<()> {
        let ct = renderer.custom_type.clone();
        if self.message_renderers.contains_key(&ct) {
            bail!("message renderer for '{}' is already registered", ct);
        }
        self.message_renderers.insert(ct, renderer);
        Ok(())
    }

    fn register_provider(&mut self, config: ProviderConfig) -> Result<()> {
        let name = config.name.clone();
        self.providers.insert(name, config);
        Ok(())
    }

    fn unregister_provider(&mut self, name: &str) -> Result<()> {
        self.providers.remove(name);
        Ok(())
    }

    // ── Visual ──────────────────────────────────────────────────────────

    fn register_theme(&mut self, theme: ThemeSpec) -> Result<()> {
        let name = theme.name.clone();
        if self.themes.contains_key(&name) {
            bail!("theme '{}' is already registered", name);
        }
        self.themes.insert(name, theme);
        Ok(())
    }

    fn register_spinner(&mut self, spinner: SpinnerSpec) -> Result<()> {
        let name = spinner.name.clone();
        if self.spinners.contains_key(&name) {
            bail!("spinner '{}' is already registered", name);
        }
        self.spinners.insert(name, spinner);
        Ok(())
    }

    fn register_thinking_expression(&mut self, expr: ThinkingExpressionSpec) -> Result<()> {
        let name = expr.name.clone();
        if self.thinking_expressions.contains_key(&name) {
            bail!("thinking expression '{}' is already registered", name);
        }
        self.thinking_expressions.insert(name, expr);
        Ok(())
    }

    fn register_text_effect(&mut self, effect: TextEffectSpec) -> Result<()> {
        let name = effect.name.clone();
        if self.text_effects.contains_key(&name) {
            bail!("text effect '{}' is already registered", name);
        }
        self.text_effects.insert(name, effect);
        Ok(())
    }

    fn register_overlay(&mut self, overlay: OverlaySpec) -> Result<()> {
        let name = overlay.name.clone();
        if self.overlays.contains_key(&name) {
            bail!("overlay '{}' is already registered", name);
        }
        self.overlays.insert(name, overlay);
        Ok(())
    }
}
