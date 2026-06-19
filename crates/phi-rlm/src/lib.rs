//! RLM engine: LanguageModel trait, root loop, limits, sub-call routing.

pub mod engine;
pub mod handler;
pub mod loop_detect;
pub mod parsing;
pub mod prompts;
pub mod provider;
pub mod turn;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use engine::{
    EnvFactory, LimitKind, PythonEnvFactory, RestoredExchange, RestoredRepl, Rlm, RlmCompletion,
    RlmConfig, RlmEvent, RlmSession, StopCause,
};
pub use handler::{Completion, DeltaSink, LanguageModel, LmHandler, ThinkingSink, UsageBreakdown};
pub use loop_detect::LoopDetector;
pub use parsing::{could_open_block, find_code_blocks, split_at_first_block, split_prose};
pub use phi_repl::ReplResult;
pub use provider::ProviderLm;
pub use turn::{HarnessError, HarnessPhase, TurnState};

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
