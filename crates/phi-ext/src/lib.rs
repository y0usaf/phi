use std::fmt;
use std::sync::Arc;

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;

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
    pub params: Vec<ToolParam>,
    #[serde(skip_serializing)]
    pub handler: ToolHandler,
}

#[derive(Clone, Debug, Serialize)]
pub struct ToolParam {
    pub name: String,
    pub ty: String,
    pub default: Option<String>,
}

#[derive(Clone, Serialize)]
pub struct CommandSpec {
    pub name: String,
    pub description: String,
    #[serde(skip_serializing)]
    pub handler: CommandHandler,
}

pub struct EventHandlerRegistration {
    pub event: EventKind,
    pub label: String,
    pub handler: EventHandler,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum EventKind {
    RuntimeStart,
    SessionStart,
    TuiReady,
    AgentStart,
    AgentEnd,
    TurnStart,
    TurnEnd,
    BeforeAgentStart,
    Context,
    ToolCall,
    ToolResult,
    Input,
    SessionShutdown,
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::RuntimeStart => "runtime_start",
            Self::SessionStart => "session_start",
            Self::TuiReady => "tui_ready",
            Self::AgentStart => "agent_start",
            Self::AgentEnd => "agent_end",
            Self::TurnStart => "turn_start",
            Self::TurnEnd => "turn_end",
            Self::BeforeAgentStart => "before_agent_start",
            Self::Context => "context",
            Self::ToolCall => "tool_call",
            Self::ToolResult => "tool_result",
            Self::Input => "input",
            Self::SessionShutdown => "session_shutdown",
        };

        f.write_str(name)
    }
}

#[derive(Clone, Debug, Serialize)]
pub enum EventPayload {
    Empty,
    SessionStart { cwd: String, session_id: String },
    BeforeAgentStart { prompt: String, system_prompt: String },
    Context { turn: u32 },
    TurnStart { turn: u32 },
    TurnEnd { turn: u32, had_code: bool },
    AgentEnd { stop_cause: String },
    ToolCall { name: String, args: Value },
    ToolResult { name: String, result: String, error: Option<String> },
    Input { text: String },
}

#[derive(Clone, Debug)]
pub struct LifecycleEvent {
    pub kind: EventKind,
    pub payload: EventPayload,
}

#[derive(Clone, Debug)]
pub enum EventReturn {
    SystemPromptMod { append: Option<String>, prepend: Option<String> },
    MessageInjection { prepend: Vec<(String, String)> },
    ToolBlock { reason: String },
    ToolResultOverride { content: String },
    InputAction { action: InputActionKind, text: Option<String> },
}

#[derive(Clone, Debug)]
pub enum InputActionKind {
    Continue,
    Transform,
    Handled,
}

pub type EventHandler = Arc<dyn Fn(LifecycleEvent) -> Result<Option<EventReturn>> + Send + Sync>;
pub type ToolHandler = Arc<dyn Fn(ToolInvocation) -> Result<ToolOutput> + Send + Sync>;
pub type CommandHandler = Arc<dyn Fn(CommandInvocation) -> Result<CommandOutput> + Send + Sync>;

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
