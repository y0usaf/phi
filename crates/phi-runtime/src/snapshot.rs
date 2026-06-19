use phi_ext::{CommandSpec, EventKind, ExtensionManifest, ToolSpec};
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct RuntimeSnapshot {
    pub cwd: String,
    pub session_id: String,
    pub extensions: Vec<ExtensionManifest>,
    pub tools: Vec<ToolSummary>,
    pub commands: Vec<CommandSummary>,
    pub event_handlers: Vec<EventHandlerSummary>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ToolSummary {
    pub name: String,
    pub description: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct CommandSummary {
    pub name: String,
    pub description: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct EventHandlerSummary {
    pub event: EventKind,
    pub label: String,
}

pub(crate) fn tool_summary(tool: &ToolSpec) -> ToolSummary {
    ToolSummary {
        name: tool.name.clone(),
        description: tool.description.clone(),
    }
}

pub(crate) fn command_summary(command: &CommandSpec) -> CommandSummary {
    CommandSummary {
        name: command.name.clone(),
        description: command.description.clone(),
    }
}
