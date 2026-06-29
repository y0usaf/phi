use crate::{CommandSpec, EventKind, ExtensionManifest, FlagSpec, ShortcutSpec, ToolSpec};
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct RuntimeSnapshot {
    pub cwd: String,
    pub session_id: String,
    pub extensions: Vec<ExtensionManifest>,
    pub tools: Vec<ToolSummary>,
    pub commands: Vec<CommandSummary>,
    pub shortcuts: Vec<ShortcutSummary>,
    pub flags: Vec<FlagSummary>,
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
pub struct ShortcutSummary {
    pub key: String,
    pub description: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct FlagSummary {
    pub name: String,
    pub description: String,
    pub flag_type: String,
    pub default: Option<String>,
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

pub(crate) fn shortcut_summary(shortcut: &ShortcutSpec) -> ShortcutSummary {
    ShortcutSummary {
        key: shortcut.key.clone(),
        description: shortcut.description.clone(),
    }
}

pub(crate) fn flag_summary(flag: &FlagSpec) -> FlagSummary {
    FlagSummary {
        name: flag.name.clone(),
        description: flag.description.clone(),
        flag_type: match flag.flag_type {
            crate::FlagType::Boolean => "boolean".to_owned(),
            crate::FlagType::String => "string".to_owned(),
        },
        default: flag.default.clone(),
    }
}
