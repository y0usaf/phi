//! Core language model types: message types, LanguageModel trait, and handler.
//!
//! The types live in `phi_ai::lm`; this module re-exports them for backward
//! compatibility within phi-rlm.

pub mod handler;

pub use handler::LmHandler;

pub use phi_ai::{
    ChatMessage, Completion, ContextPayload, DeltaSink, LanguageModel, Role, StopReason,
    ThinkingSink, ToolCall, ToolCallEvent, ToolCallSink, UsageBreakdown,
};
