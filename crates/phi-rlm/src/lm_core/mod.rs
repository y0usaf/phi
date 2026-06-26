//! Core language model types: message types, LanguageModel trait, and handler.

pub mod handler;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use handler::{
    Completion, DeltaSink, LanguageModel, LmHandler, StopReason, ThinkingSink, ToolCallEvent,
    ToolCallSink, UsageBreakdown,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    /// A tool result returned to the model, paired by `tool_call_id`.
    Tool,
}

/// A native tool call emitted by the model (as a `tool_use` / `function_call`
/// content block, not text-based fences).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    /// For `Role::Assistant`: tool calls the model emitted alongside `content`.
    /// For `Role::Tool`: empty — the tool result is in `content`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// For `Role::Tool`: the ID of the tool call this result answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Default for ChatMessage {
    fn default() -> Self {
        Self {
            role: Role::User,
            content: String::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }
}

impl ChatMessage {
    /// Shorthand for a plain text message (no tool calls).
    pub fn text(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    /// Shorthand for an assistant message with tool calls.
    pub fn assistant_with_tools(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_calls,
            tool_call_id: None,
        }
    }

    /// Shorthand for a tool result message.
    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.into()),
        }
    }
}

/// The long context injected into the REPL as `context`.
#[derive(Clone, Debug)]
pub enum ContextPayload {
    Str(String),
    Json(Value),
}
