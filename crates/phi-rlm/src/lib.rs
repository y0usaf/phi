//! RLM engine: recursive language model with a Python REPL, sub-LLM calls,
//! and multi-turn sessions.
//!
//! Modules:
//! - `lm_core`: message types + `LanguageModel` trait
//! - `llm_parse`: iteration formatting (REPL output → user message)
//! - `provider`: concrete `ProviderLm` impl
//! - `prompts`: system/user prompt rendering
//! - `config`: config, limits, events, `EnvFactory`
//! - `engine`: single-turn completion loop + sub-calls + harness state types
//! - `session`: multi-turn persistence + compaction

pub(crate) mod lm_core;

// Re-export lm_core and phi-repl types so downstream `use phi_rlm::*` works.
pub use lm_core::{
    ChatMessage, Completion, ContextPayload, DeltaSink, LanguageModel, Role, StopReason,
    ThinkingSink, ToolCall, ToolCallEvent, ToolCallSink, UsageBreakdown,
};
pub use phi_repl::ReplResult;

pub(crate) mod config;
pub(crate) mod engine;
pub(crate) mod llm_parse;
pub(crate) mod prompts;
pub(crate) mod provider;
pub(crate) mod session;

// Flat re-exports for the public API.
pub use config::{LimitKind, RlmCompletion, RlmConfig, RlmEvent, StopCause};
pub use engine::{
    HarnessError, HarnessPhase, PhiHarness, RestoredExchange, RestoredRepl, RestoredSubcall, Rlm,
};

pub use prompts::{DEFAULT_ANSWER_NUDGE, Skill, SystemPromptOptions, build_system_prompt};
pub use provider::ProviderLm;
pub use session::RlmSession;
