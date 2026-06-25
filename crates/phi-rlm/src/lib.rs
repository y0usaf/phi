//! RLM engine — thin umbrella re-exporting the focused sub-crates.
//!
//! Each concern lives in its own crate:
//! - `phi-lm-core`: message types + LanguageModel trait
//! - `phi-llm-parse`: code-block extraction and prose splitting
//! - `phi-rlm-prompts`: system/user prompt rendering
//! - `phi-rlm-config`: config, limits, events, EnvFactory
//! - `phi-rlm-engine`: single-turn completion loop + sub-calls + harness state types
//! - `phi-provider`: concrete ProviderLm impl
//! - `phi-rlm-session`: multi-turn persistence + compaction
//!
//! This crate re-exports the full public API so downstream consumers
//! (phi-app, phi-harness, phi-session) need no changes.

pub use phi_lm_core::{ChatMessage, Completion, ContextPayload, DeltaSink,
    LanguageModel, LmHandler, Role, ThinkingSink, UsageBreakdown};
pub use phi_llm_parse::{could_open_block, find_code_blocks, split_at_first_block, split_prose};
pub use phi_provider::ProviderLm;
pub use phi_repl::ReplResult;
pub use phi_rlm_engine::{HarnessError, HarnessPhase, TurnState};
pub use phi_rlm_config::{RlmConfig, RlmCompletion, RlmEvent, StopCause};
pub use phi_rlm_engine::{Rlm, RestoredExchange, RestoredRepl};
pub use phi_rlm_session::RlmSession;
