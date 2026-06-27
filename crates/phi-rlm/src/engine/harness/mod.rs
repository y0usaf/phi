//! Harness: the orchestration layer between the engine and the frontends.
//!
//! Owns the phase state machine, turn snapshots, queues, and loop detection.

pub(crate) mod config;
pub(crate) mod queue;

pub(crate) use config::HarnessConfig;
pub(crate) use queue::{QueueSource, QueuedMessage, Queues};

// Re-export phase/error types that already live in the engine module.
pub use super::HarnessError;
pub use super::HarnessPhase;
pub(crate) use super::TurnState;

use std::sync::Arc;

use phi_ai::ThinkingLevel;
use phi_ext::ToolSpec;

use crate::config::RlmCompletion;
use crate::engine::Rlm;
use crate::lm_core::{ContextPayload, LanguageModel};

use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

use phi_ext::AppRuntime;

use crate::config::RlmEvent;

/// Orchestration layer wrapping an [`Rlm`] engine.
pub struct PhiHarness {
    rlm: Rlm,
    phase: Mutex<HarnessPhase>,
    queues: Queues,
    cancel: Mutex<Option<CancellationToken>>,
    config: RwLock<HarnessConfig>,
}

impl PhiHarness {
    /// Construct a harness wrapping `rlm`, with a default [`HarnessConfig`]
    /// derived from the `Rlm`'s root/sub models.
    pub fn new(rlm: Rlm) -> Self {
        let (root, sub, config) = rlm.models_and_config();
        Self {
            rlm,
            phase: Mutex::new(HarnessPhase::Idle),
            queues: Queues::new(),
            cancel: Mutex::new(None),
            config: RwLock::new(HarnessConfig::new(root, sub, config)),
        }
    }

    pub fn with_events(mut self, sender: UnboundedSender<RlmEvent>) -> Self {
        self.rlm = self.rlm.with_events(sender);
        self
    }

    pub fn with_runtime(mut self, runtime: Arc<AppRuntime>) -> Self {
        self.rlm = self.rlm.with_runtime(runtime);
        self
    }

    /// Set the live harness config (replaces the default).
    pub(crate) fn with_config(mut self, config: HarnessConfig) -> Self {
        self.config = RwLock::new(config);
        self
    }

    pub fn rlm(&self) -> &Rlm {
        &self.rlm
    }
    pub(crate) fn queues(&self) -> &Queues {
        &self.queues
    }

    pub(crate) async fn phase(&self) -> HarnessPhase {
        *self.phase.lock().await
    }

    /// Start a new turn. Requires [`HarnessPhase::Idle`].
    pub async fn prompt(
        &self,
        payload: ContextPayload,
        root_prompt: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<RlmCompletion, HarnessError> {
        // 1. Assert idle, set phase to Turn
        {
            let mut phase = self.phase.lock().await;
            if !phase.is_idle() {
                return Err(HarnessError::Busy { phase: *phase });
            }
            *phase = HarnessPhase::Turn;
        }
        *self.cancel.lock().await = Some(cancel.clone());

        struct PhaseGuard<'a>(&'a PhiHarness);
        impl Drop for PhaseGuard<'_> {
            fn drop(&mut self) {
                if let Ok(mut phase) = self.0.phase.try_lock() {
                    *phase = HarnessPhase::Idle;
                }
            }
        }
        let _guard = PhaseGuard(self);

        // 2. Drain next_turn queue messages and prepend to the prompt.
        let next_turn = self.queues.drain_next_turn().await;
        let queued_payload = if next_turn.is_empty() {
            payload
        } else {
            let combined = next_turn
                .into_iter()
                .map(|m| m.text)
                .collect::<Vec<_>>()
                .join("\n\n");
            match &payload {
                ContextPayload::Str(s) => ContextPayload::Str(format!("{combined}\n\n{s}")),
                ContextPayload::Json(v) => ContextPayload::Str(format!("{combined}\n\n{v}")),
            }
        };

        // 3. Create TurnState snapshot from harness config
        let turn = {
            let cfg = self.config.read().await;
            TurnState {
                messages: Vec::new(),
                system_prompt: cfg.system_prompt.clone().unwrap_or_default(),
                root_model: cfg.root_model.clone(),
                sub_model: cfg.sub_model.clone(),
                thinking: cfg.thinking,
                config: cfg.config.clone(),
                session_id: String::new(),
                iteration: 0,
                tools_section: cfg.config.tools_section.clone(),
                context: queued_payload.clone(),
            }
        };

        // 4. Run the completion.
        let completion = self
            .rlm
            .completion_with(queued_payload, root_prompt, Some(&turn), cancel)
            .await;

        *self.cancel.lock().await = None;
        *self.phase.lock().await = HarnessPhase::Idle;

        Ok(completion)
    }

    /// Abort the current run. Allowed during [`HarnessPhase::Turn`].
    pub(crate) async fn abort(&self) -> Result<(), HarnessError> {
        if let Some(token) = self.cancel.lock().await.take() {
            token.cancel();
        }
        self.queues.clear_runtime_queues().await;
        Ok(())
    }

    // ── Queue ops (allowed during Turn) ─────────────────────────────────────

    pub(crate) async fn steer(&self, text: &str) -> Result<String, HarnessError> {
        Ok(self.queues.enqueue_steer(text).await)
    }
    pub(crate) async fn follow_up(&self, text: &str) -> Result<String, HarnessError> {
        Ok(self.queues.enqueue_follow_up(text).await)
    }
    pub(crate) async fn next_turn(&self, text: &str) -> Result<String, HarnessError> {
        Ok(self.queues.enqueue_next_turn(text).await)
    }

    // ── Config setters (affect next snapshot) ───────────────────────────────

    pub(crate) async fn set_model(&self, model: Arc<dyn LanguageModel>) {
        self.config.write().await.root_model = model;
    }

    pub(crate) async fn set_thinking(&self, level: Option<ThinkingLevel>) {
        self.config.write().await.thinking = level;
    }

    pub(crate) async fn set_tools(&self, tools: Vec<ToolSpec>, active: Vec<String>) {
        let mut cfg = self.config.write().await;
        cfg.tools = tools;
        cfg.active_tools = active;
    }
}
