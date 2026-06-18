use anyhow::Result;

use crate::{ExtensionManifest, ToolSpec, CommandSpec, EventHandlerRegistration};

pub trait Extension: Send + Sync {
    fn manifest(&self) -> ExtensionManifest;
    fn register(&self, host: &mut dyn ExtensionHost) -> Result<()>;
}

pub trait ExtensionHost {
    fn register_tool(&mut self, tool: ToolSpec) -> Result<()>;
    fn register_command(&mut self, command: CommandSpec) -> Result<()>;
    fn subscribe(&mut self, handler: EventHandlerRegistration) -> Result<()>;
}
