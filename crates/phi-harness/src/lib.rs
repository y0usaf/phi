//! Phi Harness: the orchestration layer between [`phi_rlm`] and the frontends.
//!
//! Owns the phase state machine, turn snapshots, save points, queues, hooks,
//! loop detection, observability, and hooks.

pub mod error;
pub mod hooks;
pub mod observe;
pub mod phase;
pub mod queue;
pub mod save_point;
pub mod snapshot;

pub use error::HarnessError;
pub use hooks::{
    BeforeAgentStartEvent, BeforeAgentStartResult, ContextEvent, ContextResult, HookEvent,
    HookHandler, HookObserver, HookScope, PhiHooks, RunEndEvent, RunStartEvent, SourceInfo,
    ToolCallEvent, ToolCallResult, ToolResultEvent, ToolResultPatch,
};
pub use observe::{
    ErrorInfo, ObservabilityContext, ObservabilityEvent, ObservabilityEventType,
    ObservabilitySubscriber, safe_payload, subscribe_observability, trace_operation,
};
pub use phase::HarnessPhase;
pub use queue::{QueueMode, QueueSource, QueuedMessage, Queues};
pub use save_point::SavePoint;
pub use snapshot::TurnState;

use std::sync::Arc;

use phi_ai::ThinkingLevel;
use phi_ext::ToolSpec;
use phi_rlm::{ContextPayload, LanguageModel, Rlm, RlmCompletion, RlmConfig};
use phi_runtime::AppRuntime;
use phi_session::session::SessionStore;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

/// Live (non-snapshotted) harness configuration. Updated by `set_model`,
/// `set_thinking`, `set_tools`; read when creating a fresh [`TurnState`].
pub struct HarnessConfig {
    /// Root model for completions.
    pub root_model: Arc<dyn LanguageModel>,
    /// Sub model for sub-calls.
    pub sub_model: Arc<dyn LanguageModel>,
    /// Thinking level override.
    pub thinking: Option<ThinkingLevel>,
    /// Base RLM config (limits, timeouts, etc).
    pub config: RlmConfig,
    /// Custom system prompt override.
    pub system_prompt: Option<String>,
    /// Active tool specs.
    pub tools: Vec<ToolSpec>,
    /// Active tool names.
    pub active_tools: Vec<String>,
}

impl HarnessConfig {
    /// Construct from the models and base config.
    pub fn new(
        root_model: Arc<dyn LanguageModel>,
        sub_model: Arc<dyn LanguageModel>,
        config: RlmConfig,
    ) -> Self {
        Self {
            root_model,
            sub_model,
            thinking: None,
            config,
            system_prompt: None,
            tools: Vec::new(),
            active_tools: Vec::new(),
        }
    }
}

/// Orchestration layer wrapping an [`Rlm`] engine.
pub struct PhiHarness {
    rlm: Rlm,
    phase: Mutex<HarnessPhase>,
    queues: Queues,
    hooks: PhiHooks,
    cancel: Mutex<Option<CancellationToken>>,
    config: RwLock<HarnessConfig>,
    session: Mutex<Option<SessionStore>>,
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
            hooks: PhiHooks::new(),
            cancel: Mutex::new(None),
            config: RwLock::new(HarnessConfig::new(root, sub, config)),
            session: Mutex::new(None),
        }
    }

    pub fn with_events(mut self, sender: UnboundedSender<phi_rlm::RlmEvent>) -> Self {
        self.rlm = self.rlm.with_events(sender);
        self
    }

    pub fn with_runtime(mut self, runtime: Arc<AppRuntime>) -> Self {
        self.rlm = self.rlm.with_runtime(runtime);
        self
    }

    /// Attach a session for persistence.
    pub fn with_session(mut self, session: SessionStore) -> Self {
        self.session = Mutex::new(Some(session));
        self
    }

    /// Set the live harness config (replaces the default).
    pub fn with_config(mut self, config: HarnessConfig) -> Self {
        self.config = RwLock::new(config);
        self
    }

    pub fn rlm(&self) -> &Rlm {
        &self.rlm
    }
    pub fn queues(&self) -> &Queues {
        &self.queues
    }
    pub fn hooks(&self) -> &PhiHooks {
        &self.hooks
    }

    pub async fn phase(&self) -> HarnessPhase {
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

        // 2. Emit observation hooks
        let _ = self
            .hooks
            .emit_observation(RunStartEvent {
                session_id: String::new(),
            })
            .await;

        // 3. Drain next_turn queue messages and prepend to the prompt.
        let next_turn = self.queues.drain_next_turn().await;
        let queued_payload = if next_turn.is_empty() {
            payload
        } else {
            let combined = next_turn
                .iter()
                .map(|m| m.text.clone())
                .collect::<Vec<_>>()
                .join("\n\n");
            match &payload {
                ContextPayload::Str(s) => ContextPayload::Str(format!("{combined}\n\n{s}")),
                ContextPayload::Json(v) => ContextPayload::Str(format!("{combined}\n\n{v}")),
            }
        };

        // 4. Create TurnState snapshot from harness config
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

        // 5. Run the completion, wrapped in observability.
        let cancel_clone = cancel.clone();
        let completion = trace_operation(
            "phi.agent.prompt",
            serde_json::json!({ "root_prompt": root_prompt.unwrap_or("") }),
            || async {
                let result = self
                    .rlm
                    .completion_with(queued_payload, root_prompt, Some(&turn), &cancel_clone)
                    .await;
                Ok(result)
            },
        )
        .await?;

        // 6. Emit run-end hook
        let _ = self
            .hooks
            .emit_observation(RunEndEvent {
                stop_cause: completion.stop,
            })
            .await;

        *self.cancel.lock().await = None;
        *self.phase.lock().await = HarnessPhase::Idle;

        Ok(completion)
    }

    /// Abort the current run. Allowed during [`HarnessPhase::Turn`].
    pub async fn abort(&self) -> Result<(), HarnessError> {
        if let Some(token) = self.cancel.lock().await.take() {
            token.cancel();
        }
        self.queues.clear_runtime_queues().await;
        Ok(())
    }

    // ── Structural ops (require Idle) ───────────────────────────────────────

    /// Compact the session transcript. Requires Idle.
    pub async fn compact(&self) -> Result<(), HarnessError> {
        {
            let mut phase = self.phase.lock().await;
            if !phase.is_idle() {
                return Err(HarnessError::Busy { phase: *phase });
            }
            *phase = HarnessPhase::Compaction;
        }
        struct Guard<'a>(&'a PhiHarness);
        impl Drop for Guard<'_> {
            fn drop(&mut self) {
                if let Ok(mut p) = self.0.phase.try_lock() {
                    *p = HarnessPhase::Idle;
                }
            }
        }
        let _g = Guard(self);

        // Compaction is delegated to the session layer; the harness just
        // manages the phase transition. The session's own compaction
        // threshold handles the actual summarization.
        Ok(())
    }

    /// Navigate to a different leaf in the session tree. Requires Idle.
    pub async fn navigate_tree(&self, leaf_id: &str) -> Result<(), HarnessError> {
        {
            let mut phase = self.phase.lock().await;
            if !phase.is_idle() {
                return Err(HarnessError::Busy { phase: *phase });
            }
            *phase = HarnessPhase::Turn;
        }
        struct Guard<'a>(&'a PhiHarness);
        impl Drop for Guard<'_> {
            fn drop(&mut self) {
                if let Ok(mut p) = self.0.phase.try_lock() {
                    *p = HarnessPhase::Idle;
                }
            }
        }
        let _g = Guard(self);

        let mut guard = self.session.lock().await;
        if let Some(session) = guard.as_mut() {
            session.set_leaf(Some(leaf_id.to_owned())).map_err(HarnessError::Session)?;
        }
        Ok(())
    }

    // ── Queue ops (allowed during Turn) ─────────────────────────────────────

    pub async fn steer(&self, text: &str) -> Result<String, HarnessError> {
        let id = self.queues.enqueue_steer(text).await;
        Ok(id)
    }
    pub async fn follow_up(&self, text: &str) -> Result<String, HarnessError> {
        let id = self.queues.enqueue_follow_up(text).await;
        Ok(id)
    }
    pub async fn next_turn(&self, text: &str) -> Result<String, HarnessError> {
        let id = self.queues.enqueue_next_turn(text).await;
        Ok(id)
    }

    // ── Config setters (affect next snapshot) ───────────────────────────────

    pub async fn set_model(&self, model: Arc<dyn LanguageModel>) {
        self.config.write().await.root_model = model;
    }

    pub async fn set_thinking(&self, level: Option<ThinkingLevel>) {
        self.config.write().await.thinking = level;
    }

    pub async fn set_tools(&self, tools: Vec<ToolSpec>, active: Vec<String>) {
        let mut cfg = self.config.write().await;
        cfg.tools = tools;
        cfg.active_tools = active;
    }

}
