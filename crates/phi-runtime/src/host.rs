use std::collections::BTreeMap;

use anyhow::{Result, bail};
use phi_ext::{CommandSpec, EventHandlerRegistration, ExtensionHost, ToolSpec};

#[derive(Default)]
pub(crate) struct RegistryHost {
    pub(crate) tools: BTreeMap<String, ToolSpec>,
    pub(crate) commands: BTreeMap<String, CommandSpec>,
    pub(crate) event_handlers: Vec<EventHandlerRegistration>,
}

impl ExtensionHost for RegistryHost {
    fn register_tool(&mut self, tool: ToolSpec) -> Result<()> {
        if self.tools.contains_key(&tool.name) {
            bail!("tool '{}' is already registered", tool.name);
        }
        self.tools.insert(tool.name.clone(), tool);
        Ok(())
    }

    fn register_command(&mut self, command: CommandSpec) -> Result<()> {
        if self.commands.contains_key(&command.name) {
            bail!("command '{}' is already registered", command.name);
        }
        self.commands.insert(command.name.clone(), command);
        Ok(())
    }

    fn subscribe(&mut self, handler: EventHandlerRegistration) -> Result<()> {
        self.event_handlers.push(handler);
        Ok(())
    }
}
