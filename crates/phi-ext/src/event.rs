use std::fmt;
use std::sync::Arc;

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;

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
    SessionStart {
        cwd: String,
        session_id: String,
    },
    BeforeAgentStart {
        prompt: String,
        system_prompt: String,
    },
    Context {
        turn: u32,
    },
    TurnStart {
        turn: u32,
    },
    TurnEnd {
        turn: u32,
        had_code: bool,
    },
    AgentEnd {
        stop_cause: String,
    },
    ToolCall {
        name: String,
        args: Value,
    },
    ToolResult {
        name: String,
        result: String,
        error: Option<String>,
    },
    Input {
        text: String,
    },
}

#[derive(Clone, Debug)]
pub struct LifecycleEvent {
    pub kind: EventKind,
    pub payload: EventPayload,
}

#[derive(Clone, Debug)]
pub enum EventReturn {
    SystemPromptMod {
        append: Option<String>,
        prepend: Option<String>,
    },
    MessageInjection {
        prepend: Vec<(String, String)>,
    },
    ToolBlock {
        reason: String,
    },
    ToolResultOverride {
        content: String,
    },
    InputAction {
        action: InputActionKind,
        text: Option<String>,
    },
}

#[derive(Clone, Debug)]
pub enum InputActionKind {
    Continue,
    Transform,
    Handled,
}

pub struct EventHandlerRegistration {
    pub event: EventKind,
    pub label: String,
    pub handler: EventHandler,
}

pub type EventHandler = Arc<dyn Fn(LifecycleEvent) -> Result<Option<EventReturn>> + Send + Sync>;
