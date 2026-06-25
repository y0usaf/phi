//! Core language model types: message types, LanguageModel trait, and handler.

pub mod handler;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use handler::{Completion, DeltaSink, LanguageModel, LmHandler, ThinkingSink, UsageBreakdown};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

/// The long context injected into the REPL as `context`.
#[derive(Clone, Debug)]
pub enum ContextPayload {
    Str(String),
    Json(Value),
}
