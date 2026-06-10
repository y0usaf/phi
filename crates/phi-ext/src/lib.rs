use std::fmt;
use std::sync::Arc;

use anyhow::Result;
use serde::Serialize;

pub trait Extension: Send + Sync {
    fn manifest(&self) -> ExtensionManifest;
    fn register(&self, host: &mut dyn ExtensionHost) -> Result<()>;
}

pub trait ExtensionHost {
    fn register_tool(&mut self, tool: ToolSpec) -> Result<()>;
    fn register_command(&mut self, command: CommandSpec) -> Result<()>;
    fn subscribe(&mut self, handler: EventHandlerRegistration) -> Result<()>;
}

#[derive(Clone, Debug, Serialize)]
pub struct ExtensionManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
}

#[derive(Clone, Serialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    #[serde(skip_serializing)]
    pub handler: ToolHandler,
}

#[derive(Clone, Serialize)]
pub struct CommandSpec {
    pub name: String,
    pub description: String,
    #[serde(skip_serializing)]
    pub handler: CommandHandler,
}

#[derive(Clone)]
pub struct EventHandlerRegistration {
    pub event: EventKind,
    pub label: String,
    pub handler: EventHandler,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum EventKind {
    RuntimeStart,
    SessionStart,
    BeforeModel,
    AfterModel,
    BeforeTool,
    AfterTool,
    TuiReady,
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::RuntimeStart => "runtime_start",
            Self::SessionStart => "session_start",
            Self::BeforeModel => "before_model",
            Self::AfterModel => "after_model",
            Self::BeforeTool => "before_tool",
            Self::AfterTool => "after_tool",
            Self::TuiReady => "tui_ready",
        };

        f.write_str(name)
    }
}

pub type EventHandler = Arc<dyn Fn(&EventContext, &LifecycleEvent) -> Result<()> + Send + Sync>;
pub type ToolHandler = Arc<dyn Fn(ToolInvocation) -> Result<ToolOutput> + Send + Sync>;
pub type CommandHandler = Arc<dyn Fn(CommandInvocation) -> Result<CommandOutput> + Send + Sync>;

#[derive(Clone, Debug)]
pub struct EventContext {
    pub cwd: String,
    pub session_id: String,
}

#[derive(Clone, Debug)]
pub struct LifecycleEvent {
    pub kind: EventKind,
    pub summary: String,
}

#[derive(Clone, Debug)]
pub struct ToolInvocation {
    pub input: String,
}

#[derive(Clone, Debug)]
pub struct ToolOutput {
    pub content: String,
}

#[derive(Clone, Debug)]
pub struct CommandInvocation {
    pub raw_args: String,
}

#[derive(Clone, Debug)]
pub struct CommandOutput {
    pub message: Option<String>,
}
