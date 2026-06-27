use std::collections::BTreeMap;

use crate::{CommandSpec, EventHandlerRegistration, ExtensionHost, ToolSpec};
use anyhow::{Result, bail};

#[derive(Default)]
pub(crate) struct RegistryHost {
    pub(crate) tools: BTreeMap<String, ToolSpec>,
    pub(crate) commands: BTreeMap<String, CommandSpec>,
    pub(crate) event_handlers: Vec<EventHandlerRegistration>,
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

    fn subscribe(&mut self, handler: EventHandlerRegistration) -> Result<()> {
        self.event_handlers.push(handler);
        Ok(())
    }
}
