//! Language model abstraction: message types, the `LanguageModel` trait,
//! streaming sink types, and completion/usage accumulators.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::types::{StopReason, ToolCall, ToolDefinition, Usage};

/// The role of a chat message participant.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    /// A tool result returned to the model, paired by `tool_call_id`.
    Tool,
}

/// A single message in a language-model conversation.
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

// Why the model stopped generating.
// (Re-exported from crate::StopReason — establishes the canonical home here.)

/// A completed language-model generation.
#[derive(Clone, Debug, Default)]
pub struct Completion {
    pub text: String,
    pub usage: Usage,
    /// Tool calls the model emitted (native `tool_use` blocks), if any.
    pub tool_calls: Vec<ToolCall>,
    /// Why the model stopped generating.
    pub stop_reason: StopReason,
}

/// Per-model usage breakdown for a set of completions.
#[derive(Clone, Debug, PartialEq)]
pub struct UsageBreakdown {
    pub model: String,
    pub usage: Usage,
}

/// Receives streamed text chunks during a completion. Returns `false` to
/// stop generation early (the model drops the stream and returns the text
/// accumulated so far).
pub type DeltaSink = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// Receives streamed provider-thinking chunks during a completion.
/// Observational only: thinking never affects generation control flow.
pub type ThinkingSink = Arc<dyn Fn(&str) + Send + Sync>;

/// Receives streamed tool-call events during a completion.
/// `ToolCallStart` fires when a tool call begins, `ToolCallDelta` streams
/// partial argument JSON, and `ToolCallEnd` fires when arguments are complete.
pub type ToolCallSink = Arc<dyn Fn(ToolCallEvent) + Send + Sync>;

/// A streamed tool-call event.
#[derive(Clone, Debug)]
pub enum ToolCallEvent {
    Start {
        id: String,
        name: String,
    },
    Delta {
        id: String,
        delta: String,
    },
    End {
        id: String,
        name: String,
        arguments: serde_json::Value,
    },
}

/// Implemented by plain provider-backed models (phi-ext wires phi-ai in)
/// and by `Rlm` itself — recursion is just trait recursion.
#[async_trait]
pub trait LanguageModel: Send + Sync {
    fn name(&self) -> String;
    async fn complete(
        &self,
        prompt: &[ChatMessage],
        cancel: &CancellationToken,
    ) -> Result<Completion, String>;

    /// Stream text deltas through `on_delta` and tool-call events through
    /// `on_tool_call` while completing. Models without streaming support fall
    /// back to a single non-streamed completion.
    async fn complete_streaming(
        &self,
        prompt: &[ChatMessage],
        on_delta: DeltaSink,
        on_thinking: Option<ThinkingSink>,
        on_tool_call: Option<ToolCallSink>,
        cancel: &CancellationToken,
    ) -> Result<Completion, String> {
        let _ = (on_delta, on_thinking, on_tool_call);
        self.complete(prompt, cancel).await
    }

    /// Set the tool definitions the model can call natively. Models that
    /// don't support native tool calls (e.g. scripted test models) ignore this.
    fn set_tools(&self, _tools: Vec<ToolDefinition>) {
        // Default: no-op. Overridden by ProviderLm.
    }

    /// The model's context window in tokens, when known. Drives in-loop
    /// compaction: `None` (the default) disables the threshold check.
    fn context_window(&self) -> Option<u64> {
        None
    }
}
