//! Harness-phase / turn-snapshot types.
//!
//! These live in the `engine` module (rather than `phi-harness`) to avoid
//! a dependency cycle: `phi-harness` depends on `phi-rlm`, so types referenced
//! by `Rlm::completion_with` must be defined here.

use std::fmt;
use std::sync::Arc;

use phi_ai::ThinkingLevel;

use crate::lm_core::{ChatMessage, ContextPayload, LanguageModel};
use crate::config::RlmConfig;

/// Operational phase of the harness.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HarnessPhase {
    Idle,
    Turn,
    Compaction,
    Retry,
}

impl HarnessPhase {
    pub fn is_idle(self) -> bool {
        self == Self::Idle
    }
}

/// Normalized harness error.
#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    #[error("harness is busy (phase: {phase:?}), cannot perform structural operation")]
    Busy { phase: HarnessPhase },

    #[error("hook error: {message}")]
    Hook {
        message: String,
        #[source]
        source: anyhow::Error,
    },

    #[error("session error: {0}")]
    Session(#[source] anyhow::Error),

    #[error("compaction error: {0}")]
    Compaction(#[source] anyhow::Error),

    #[error("engine error: {0}")]
    Engine(#[source] anyhow::Error),

    #[error("recovery error: {message}")]
    Recovery {
        message: String,
        #[source]
        source: anyhow::Error,
    },
}

/// Immutable snapshot of everything one RLM iteration needs.
#[derive(Clone)]
pub struct TurnState {
    pub messages: Vec<ChatMessage>,
    pub system_prompt: String,
    pub root_model: Arc<dyn LanguageModel>,
    pub sub_model: Arc<dyn LanguageModel>,
    pub thinking: Option<ThinkingLevel>,
    pub config: RlmConfig,
    pub session_id: String,
    pub iteration: usize,
    pub tools_section: Option<String>,
    pub context: ContextPayload,
}

impl TurnState {
    pub fn with_queued_messages(mut self, queued: Vec<ChatMessage>) -> Self {
        self.messages.extend(queued);
        self
    }
}

impl fmt::Debug for TurnState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TurnState")
            .field("system_prompt", &self.system_prompt)
            .field("root_model", &self.root_model.name())
            .field("sub_model", &self.sub_model.name())
            .field("thinking", &self.thinking)
            .field("config", &self.config)
            .field("session_id", &self.session_id)
            .field("iteration", &self.iteration)
            .field("tools_section", &self.tools_section)
            .field("messages_len", &self.messages.len())
            .finish()
    }
}


pub mod helpers;
pub mod rlm;
pub mod state;
pub mod subcalls;

pub use helpers::{RestoredExchange, RestoredRepl};
pub use rlm::Rlm;

// Re-export config types for convenience
pub use crate::config::{RlmCompletion, RlmEvent, StopCause};
