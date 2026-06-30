//! Persistent multi-turn RLM session.

use std::sync::Arc;
use std::time::Instant;

use phi_ai::Usage;
use phi_repl::{ReplEnv, ReplResult};
use tokio_util::sync::CancellationToken;

use phi_ext::AppRuntime;
use phi_ext::{EventKind, EventPayload, EventReturn, InputActionKind, LifecycleEvent};

use crate::llm_parse::{ExecutedBlock, format_iteration};
use crate::lm_core::LmHandler;
use crate::lm_core::{ChatMessage, ContextPayload, Role};
use crate::prompts::QueryMetadata;

use crate::config::{RlmCompletion, RlmConfig, RlmEvent, StopCause};
use crate::engine::Rlm;
use crate::engine::helpers::{
    RestoredExchange, RestoredRepl, context_snippet, dispatch_event, history_snippet,
    push_repl_turns, subcalls_snippet,
};
use crate::engine::state::RunState;
use crate::engine::subcalls::Outcome;

pub struct RlmSession {
    pub rlm: Rlm,
    pub payload: ContextPayload,
    pub handler: Arc<LmHandler>,
    pub run: RunState,
    pub env: Option<Box<dyn ReplEnv>>,
    pub exchanges: usize,
    pub contexts: usize,
    /// In-band conversational transcript: prior (user prompt, final answer)
    /// pairs replayed verbatim on every warm `send`, so follow-ups are real
    /// continuations instead of cold starts. Full per-run trajectories still
    /// land in the env as `history_N` variables (the archive tier).
    pub transcript: Vec<ChatMessage>,
    /// Compacted summary of older exchanges that have been evicted from
    /// `transcript` to keep the in-band context bounded. Injected as a
    /// user message at the top of the warm replay so the model retains
    /// continuity without re-reading every prior turn verbatim.
    pub session_summary: Option<String>,
    /// User-initiated shell executions to replay to the model on the next
    /// `send`, as (assistant ```repl``` block, REPL-output user message)
    /// pairs — the model sees them as its own prior repl turns.
    pub pending_shell: Vec<ChatMessage>,
}

/// Threshold: once the in-band transcript exceeds this many message pairs,
/// older exchanges are compacted into `session_summary`. Keeps the warm
/// replay bounded so long sessions don't grow linearly in tokens.
pub const TRANSCRIPT_COMPACTION_THRESHOLD: usize = 20;

impl RlmSession {
    /// Create a persistent multi-turn session from an RLM engine and context payload.
    pub fn new(rlm: Rlm, payload: ContextPayload) -> Self {
        let handler = rlm.handler();
        Self {
            rlm,
            payload,
            handler,
            run: RunState::new(),
            env: None,
            exchanges: 0,
            contexts: 1,
            transcript: Vec::new(),
            session_summary: None,
            pending_shell: Vec::new(),
        }
    }
    pub fn with_runtime(mut self, runtime: Arc<AppRuntime>) -> Self {
        self.rlm = self.rlm.with_runtime(runtime);
        self
    }

    pub fn usage(&self) -> Usage {
        self.handler.usage()
    }

    pub fn config(&self) -> &RlmConfig {
        &self.rlm.config
    }

    pub fn root_name(&self) -> String {
        self.handler.root_name()
    }

    pub async fn send(&mut self, prompt: &str, cancel: &CancellationToken) -> RlmCompletion {
        let start = Instant::now();
        let usage_before = self.handler.usage();
        let usage_by_model_before = self.handler.usage_by_model();
        self.rlm.events.emit(RlmEvent::RunStart { depth: 0 });
        dispatch_event(
            &self.rlm.runtime,
            LifecycleEvent {
                kind: EventKind::AgentStart,
                payload: EventPayload::Empty,
            },
        );

        // ── Input event interception ───────────────────────────────────────
        // Extensions can transform the user prompt or handle it entirely
        // (cancel the agent loop). This mirrors pi's `input` event.
        let mut prompt = prompt.to_owned();
        if let Some(rt) = &self.rlm.runtime {
            // Race the Input dispatch against cancellation. This block runs
            // before `run.begin`, so without this race a blocked Input handler
            // would make the turn uncancelable. On cancel, yield empty returns
            // (no transform/handled) and fall through — `run_loop`'s
            // `cancel.is_cancelled()` check returns the cancelled outcome.
            let returns = tokio::select! {
                r = rt.dispatch(LifecycleEvent {
                    kind: EventKind::Input,
                    payload: EventPayload::Input {
                        text: prompt.clone(),
                        source: Some("interactive".to_owned()),
                    },
                }) => r.unwrap_or_default(),
                () = cancel.cancelled() => Vec::new(),
            };
            for r in returns {
                match r {
                    EventReturn::InputAction {
                        action,
                        text,
                        images: _,
                    } => {
                        match action {
                            InputActionKind::Transform => {
                                if let Some(t) = text {
                                    prompt = t;
                                }
                            }
                            InputActionKind::Handled => {
                                // Extension handled the input; abort the agent loop.
                                return self.rlm.complete_run_since(
                                    Outcome {
                                        stop: StopCause::Handled,
                                        response: String::new(),
                                        error: None,
                                        iterations: 0,
                                    },
                                    &self.handler,
                                    &usage_before,
                                    &usage_by_model_before,
                                    start,
                                );
                            }
                            InputActionKind::Continue => {}
                        }
                    }
                    _ => {}
                }
            }
        }

        self.run.begin(cancel.clone());

        let fresh_env = self.env.is_none();
        if let Err(message) = self.ensure_env().await {
            return self.rlm.complete_run_since(
                Outcome::failed(message, 0),
                &self.handler,
                &usage_before,
                &usage_by_model_before,
                start,
            );
        }
        if !fresh_env {
            let env = self.env.as_mut().expect("env started above");
            let _ = env
                .exec(
                    "answer['content'] = ''\nanswer['ready'] = False",
                    self.rlm.config.exec_timeout,
                )
                .await;
        }

        // Load the user's prompt as `context_N` into the REPL, mirroring
        // upstream RLM persistent-mode `add_context(prompt)`. The model can
        // slice and feed the prompt to sub-LLMs just like any other context.
        // Best-effort: a failure here is non-fatal — the prompt is still in
        // the transcript as "Answer the following: {prompt}".
        let _ = self.add_context(&prompt).await;

        let env = self.env.as_mut().expect("env started above");
        let metadata = QueryMetadata::from_payload(&self.payload);
        let mut history = if self.transcript.is_empty() {
            self.rlm.initial_history(metadata, Some(&prompt))
        } else {
            // Warm send: replay the conversation so far in-band, then append
            // the new prompt as its own user message. If older exchanges were
            // compacted into `session_summary`, inject that summary first so
            // the model retains continuity without re-reading every old turn.
            let mut history = self.rlm.initial_history(metadata, None);
            if let Some(summary) = &self.session_summary {
                history.push(ChatMessage {
                    role: Role::User,
                    content: format!(
                        "Session summary (older conversation compacted for context):
{summary}"
                    ),
                    ..Default::default()
                });
                history.push(ChatMessage {
                    role: Role::Assistant,
                    content: "Understood — I have the session summary and will build on it."
                        .to_owned(),
                    ..Default::default()
                });
            }
            history.extend(self.transcript.iter().cloned());
            history.append(&mut self.pending_shell);
            history.push(ChatMessage {
                role: Role::User,
                content: format!("Answer the following: {prompt}"),
                ..Default::default()
            });
            history
        };
        if let Some(rt) = &self.rlm.runtime
            && let Some(sys_msg) = history.iter_mut().find(|m| m.role == Role::System)
        {
            // Race the system-prompt filter against cancellation. Bind the
            // original first so the future and the cancel fallback each own a
            // clone, isolating the mutable borrow of `sys_msg` to the final
            // assignment.
            let original = sys_msg.content.clone();
            let filtered = tokio::select! {
                r = rt.filter_system_prompt(&prompt, original.clone()) => {
                    r.unwrap_or_else(|_| original.clone())
                }
                () = cancel.cancelled() => original.clone(),
            };
            sys_msg.content = filtered;
        }
        // Drain pending shell executions for cold sends (warm sends already
        // drained above). This is a no-op for warm sends because Vec::append
        // empties the source vector.
        history.append(&mut self.pending_shell);
        let warm = !self.transcript.is_empty();
        self.rlm.register_repl_tool();
        let outcome = self
            .rlm
            .run_loop(
                env,
                &self.handler,
                &mut history,
                self.contexts,
                self.exchanges,
                warm,
                cancel,
                None,
            )
            .await;

        if !outcome.response.trim().is_empty() && outcome.stop != StopCause::Failed {
            let snippet = history_snippet(self.exchanges, &history, &outcome.response);
            let _ = env.exec(&snippet, self.rlm.config.exec_timeout).await;
            self.exchanges += 1;
            self.transcript.push(ChatMessage {
                role: Role::User,
                content: format!("Answer the following: {prompt}"),
                ..Default::default()
            });
            self.transcript.push(ChatMessage {
                role: Role::Assistant,
                content: outcome.response.clone(),
                ..Default::default()
            });
            // Compaction: once the in-band transcript exceeds the threshold,
            // fold the oldest exchange pairs into `session_summary` so the
            // warm replay stays bounded. The full trajectory is still in
            // `history_N` (the archive tier) — only the in-band copy is
            // compacted.
            self.compact_transcript().await;
        }
        self.rlm.complete_run_since(
            outcome,
            &self.handler,
            &usage_before,
            &usage_by_model_before,
            start,
        )
    }

    /// Execute user-initiated code in the session REPL (e.g. a `!command`
    /// shell escape). With `record`, the execution is replayed to the model
    /// on the next `send` formatted exactly like one of its own repl turns.
    pub async fn exec_shell(&mut self, code: &str, record: bool) -> Result<ReplResult, String> {
        // Refresh the run state so subcalls issued by user code don't see a
        // cancelled token left over from an interrupted `send`.
        self.run.begin(CancellationToken::new());
        self.ensure_env().await?;
        let env = self.env.as_mut().expect("env started above");
        let result = env
            .exec(code, self.rlm.config.exec_timeout)
            .await
            .map_err(|e| e.to_string())?;
        if record {
            self.pending_shell.push(ChatMessage {
                role: Role::Assistant,
                content: format!("```repl\n{code}\n```"),
                ..Default::default()
            });
            let block = ExecutedBlock {
                code: code.to_owned(),
                result: result.clone(),
            };
            if let Some(outputs) = format_iteration(
                std::slice::from_ref(&block),
                self.rlm.config.max_output_chars,
            ) {
                self.pending_shell.push(ChatMessage {
                    role: Role::User,
                    content: outputs,
                    ..Default::default()
                });
            }
        }
        Ok(result)
    }

    /// Load an additional payload into the REPL as `context_N`, mirroring
    /// upstream persistent-mode `add_context`. Returns the index used.
    pub async fn add_context(&mut self, text: &str) -> Result<usize, String> {
        self.ensure_env().await?;
        let index = self.contexts;
        let snippet = context_snippet(index, text);
        let env = self.env.as_mut().expect("env started above");
        env.exec(&snippet, self.rlm.config.exec_timeout)
            .await
            .map_err(|e| format!("failed to load context_{index}: {e}"))?;
        self.contexts += 1;
        Ok(index)
    }

    async fn ensure_env(&mut self) -> Result<(), String> {
        if self.env.is_none() {
            let env = self
                .rlm
                .start_env(&self.handler, self.payload.clone(), self.run.clone())
                .await?;
            self.env = Some(env);
        }
        Ok(())
    }

    /// Rebuild session state from the canonical persisted stream — resuming
    /// a session from disk. Replaces the current env and history. Each
    /// exchange's `history_N` holds the same trajectory the model saw live:
    /// the user prompt, every executed ```repl block as an assistant turn
    /// followed by its formatted output as a user turn, then the final
    /// response. Trailing user shell executions land in `pending_shell`,
    /// replayed to the model on the next send exactly as live.
    pub async fn restore_session(
        &mut self,
        exchanges: &[RestoredExchange],
        pending: &[RestoredRepl],
    ) -> Result<(), String> {
        self.reset().await;
        if exchanges.is_empty() && pending.is_empty() {
            return Ok(());
        }
        self.ensure_env().await?;
        let env = self.env.as_mut().expect("env started above");
        for exchange in exchanges {
            let mut history = Vec::new();
            // Shell escapes recorded between the previous exchange and this
            // prompt: live, they replay ahead of the new user message.
            for repl in &exchange.pre_shell {
                push_repl_turns(&mut history, repl, self.rlm.config.max_output_chars);
            }
            history.push(ChatMessage {
                role: Role::User,
                content: format!("Answer the following: {}", exchange.prompt),
                ..Default::default()
            });
            for repl in &exchange.repls {
                push_repl_turns(&mut history, repl, self.rlm.config.max_output_chars);
            }
            let snippet = history_snippet(self.exchanges, &history, &exchange.response);
            env.exec(&snippet, self.rlm.config.exec_timeout)
                .await
                .map_err(|e| format!("failed to restore history_{}: {e}", self.exchanges))?;
            // Inject subcalls_N so the model can recover full subagent
            // output without re-calling. Only created when the exchange had
            // subcalls; empty exchanges get no variable (no bloat). The
            // variable is NOT injected into the LM transcript — the model
            // must explicitly index it to see the content (gatekeeper).
            if let Some(sub_snippet) = subcalls_snippet(self.exchanges, &exchange.repls) {
                let _ = env.exec(&sub_snippet, self.rlm.config.exec_timeout).await;
            }
            self.exchanges += 1;
            self.transcript.push(ChatMessage {
                role: Role::User,
                content: format!("Answer the following: {}", exchange.prompt),
                ..Default::default()
            });
            self.transcript.push(ChatMessage {
                role: Role::Assistant,
                content: exchange.response.clone(),
                ..Default::default()
            });
        }
        for repl in pending {
            push_repl_turns(
                &mut self.pending_shell,
                repl,
                self.rlm.config.max_output_chars,
            );
        }
        // Restored sessions may exceed the compaction threshold; compact
        // so the first warm send after restore has a bounded in-band context.
        self.compact_transcript().await;
        Ok(())
    }

    /// Discard the env and conversation history (`/new`). Usage totals are
    /// kept.
    pub async fn reset(&mut self) {
        if let Some(mut env) = self.env.take() {
            env.shutdown().await;
        }
        self.exchanges = 0;
        self.contexts = 1;
        self.transcript.clear();
        self.session_summary = None;
        self.pending_shell.clear();
    }

    /// Compact the in-band transcript when it exceeds the threshold. The
    /// oldest exchange pairs are folded into `session_summary` as a concise
    /// text recap. The full per-run trajectories remain in `history_N`
    /// (the archive tier); only the verbatim in-band copy is compacted.
    ///
    /// Compaction is synchronous and deterministic — no sub-LLM call — so
    /// it is cheap and predictable. Each compacted exchange contributes one
    /// line to the summary. When the summary already exists, new lines are
    /// appended so older summaries are preserved.
    async fn compact_transcript(&mut self) {
        let pairs = self.transcript.len() / 2;
        if pairs <= TRANSCRIPT_COMPACTION_THRESHOLD {
            return;
        }

        // ── Extension: SessionBeforeCompact ──────────────────────────────
        // Let extensions cancel compaction or supply custom instructions.
        let mut from_extension = false;
        if let Some(rt) = &self.rlm.runtime {
            match rt.before_compact(None).await {
                Ok(Some(instructions)) => {
                    from_extension = !instructions.is_empty();
                }
                Ok(None) => {
                    // Extension cancelled compaction.
                    return;
                }
                Err(_) => {}
            }
        }

        // Keep the most recent threshold/2 pairs in-band; compact the rest.
        let keep_pairs = TRANSCRIPT_COMPACTION_THRESHOLD / 2;
        let compact_count = pairs - keep_pairs;
        let cut = compact_count * 2;

        let mut compacted = self.session_summary.take().unwrap_or_default();
        for chunk in self.transcript[..cut].chunks(2) {
            let user_text = chunk.first().map(|m| m.content.as_str()).unwrap_or("");
            let asst_text = chunk.get(1).map(|m| m.content.as_str()).unwrap_or("");
            // Extract the prompt after "Answer the following: "
            let prompt = user_text
                .strip_prefix("Answer the following: ")
                .unwrap_or(user_text);
            // Truncate to keep the summary concise
            let prompt_preview: String = prompt.chars().take(120).collect();
            let response_preview: String = asst_text.chars().take(200).collect();
            compacted.push_str(&format!(
                "- User: {prompt_preview}
  Assistant: {response_preview}
"
            ));
        }

        self.transcript.drain(..cut);
        self.session_summary = Some(compacted);

        // ── Extension: SessionCompact (post-compaction notification) ──────
        // Fire-and-forget: observational notification, detached so a slow
        // handler can't delay compaction returning.
        if let Some(rt) = &self.rlm.runtime {
            let rt = Arc::clone(rt);
            tokio::spawn(async move {
                let _ = rt.on_session_compact(from_extension).await;
            });
        }
    }

    pub async fn shutdown(&mut self) {
        if let Some(mut env) = self.env.take() {
            env.shutdown().await;
        }
    }
}
