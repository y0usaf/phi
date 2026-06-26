//! RLM engine: recursive language model with a Python REPL, sub-LLM calls,
//! and multi-turn sessions.
//!
//! Modules:
//! - `lm_core`: message types + `LanguageModel` trait
//! - `llm_parse`: code-block extraction and prose splitting
//! - `provider`: concrete `ProviderLm` impl
//! - `prompts`: system/user prompt rendering
//! - `config`: config, limits, events, `EnvFactory`
//! - `engine`: single-turn completion loop + sub-calls + harness state types
//! - `session`: multi-turn persistence + compaction

pub mod lm_core;

// Re-export lm_core and phi-repl types so downstream `use phi_rlm::*` works.
pub use lm_core::{
    ChatMessage, Completion, ContextPayload, DeltaSink, LanguageModel, LmHandler, Role,
    StopReason, ThinkingSink, ToolCall, ToolCallEvent, ToolCallSink, UsageBreakdown,
};
pub use phi_repl::ReplResult;

pub mod config;
pub mod llm_parse;
pub mod provider;
pub mod prompts;
pub mod engine;
pub mod session;

// Flat re-exports for the public API (preserving the old umbrella surface).
pub use config::{
    EnvFactory, Events, LimitKind, PythonEnvFactory, RlmCompletion, RlmConfig, RlmEvent,
    SavePointFn, StopCause,
};
pub use llm_parse::{
    could_open_block, find_code_blocks, split_at_first_block, split_prose,
};
pub use provider::ProviderLm;
pub use prompts::{
    ContextFile, QueryMetadata, Skill, SystemPromptOptions, build_system_prompt,
    build_user_prompt, metadata_user_prompt, DEFAULT_ANSWER_NUDGE, DEFAULT_CYBER_RISK,
    DEFAULT_IDENTITY, DEFAULT_MECHANICS, DEFAULT_REPL_TOOLS, ORCHESTRATOR_ADDENDUM,
    TURN_SCARCITY_NUDGE,
};
pub use engine::{HarnessError, HarnessPhase, Rlm, TurnState, RestoredExchange, RestoredRepl};
pub use session::RlmSession;
