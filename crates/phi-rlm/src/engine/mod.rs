//! Engine: single-turn completion loop, sub-calls, harness state types,
//! and the orchestration layer (harness).

use std::fmt;
use std::sync::Arc;

use phi_ai::ThinkingLevel;

use crate::config::RlmConfig;
use crate::lm_core::{ChatMessage, ContextPayload, LanguageModel};

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
#[allow(dead_code)]
pub(crate) struct TurnState {
    pub(crate) messages: Vec<ChatMessage>,
    pub(crate) system_prompt: String,
    pub(crate) root_model: Arc<dyn LanguageModel>,
    pub(crate) sub_model: Arc<dyn LanguageModel>,
    pub(crate) thinking: Option<ThinkingLevel>,
    pub(crate) config: RlmConfig,
    pub(crate) session_id: String,
    pub(crate) iteration: usize,
    pub(crate) tools_section: Option<String>,
    pub(crate) context: ContextPayload,
}

#[allow(dead_code)]
impl TurnState {
    pub(crate) fn with_queued_messages(mut self, queued: Vec<ChatMessage>) -> Self {
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

pub mod harness;
pub(crate) mod helpers;
pub(crate) mod rlm;
pub(crate) mod state;
pub(crate) mod subcalls;

pub use harness::PhiHarness;
pub use helpers::{RestoredExchange, RestoredRepl, RestoredSubcall};
pub use rlm::Rlm;
