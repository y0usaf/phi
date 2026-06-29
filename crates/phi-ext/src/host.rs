use std::collections::BTreeMap;

use crate::{
    CommandSpec, EventHandlerRegistration, ExtensionHost, FlagSpec, MessageRendererSpec,
    ProviderConfig, ShortcutSpec, ToolSpec,
};
use anyhow::{Result, bail};

#[derive(Default)]
pub(crate) struct RegistryHost {
    pub(crate) tools: BTreeMap<String, ToolSpec>,
    pub(crate) commands: BTreeMap<String, CommandSpec>,
    pub(crate) shortcuts: Vec<ShortcutSpec>,
    pub(crate) flags: BTreeMap<String, FlagSpec>,
    pub(crate) event_handlers: Vec<EventHandlerRegistration>,
    pub(crate) message_renderers: BTreeMap<String, MessageRendererSpec>,
    pub(crate) providers: BTreeMap<String, ProviderConfig>,
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
        let name = command.name.clone();
        match self.commands.entry(name) {
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
}
