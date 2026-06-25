//! Rlm struct + impl: the main engine loop.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use phi_ai::Usage;
use phi_repl::{EnvSetup, ReplEnv, ReplResult, SubcallHandler};
use serde_json::Value;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio_util::sync::CancellationToken;

use phi_ext::{EventKind, EventPayload, LifecycleEvent};
use phi_runtime::AppRuntime;

use crate::handler::{DeltaSink, LanguageModel, LmHandler, ThinkingSink, UsageBreakdown};
use crate::parsing::{ExecutedBlock, could_open_block, find_code_blocks, format_iteration, last_block_end};
use crate::prompts::{DEFAULT_ANSWER_NUDGE, PROSE_NUDGE, QueryMetadata, SystemPromptOptions, build_system_prompt, build_user_prompt, metadata_user_prompt};
use crate::{ChatMessage, ContextPayload, Role};

use super::config::{EnvFactory, Events, LimitKind, PythonEnvFactory, RlmCompletion, RlmConfig, RlmEvent, SavePointFn, StopCause};
use super::helpers::{dispatch_event, usage_breakdowns_for_model};
use super::state::RunState;
use super::subcalls::{EngineSubcalls, Outcome};
use super::session::RlmSession;

#[derive(Clone)]
pub struct Rlm {
    pub(super) config: RlmConfig,
    pub(super) root: Arc<dyn LanguageModel>,
    pub(super) sub: Arc<dyn LanguageModel>,
    pub(super) named: HashMap<String, Arc<dyn LanguageModel>>,
    pub(super) python: String,
    pub(super) host_fns: Option<Arc<dyn SubcallHandler>>,
    pub(super) events: Events,
    pub(super) depth: usize,
    pub(super) runtime: Option<Arc<AppRuntime>>,
}

impl Rlm {
    pub fn new(
        config: RlmConfig,
        root: Arc<dyn LanguageModel>,
        sub: Arc<dyn LanguageModel>,
        python: String,
    ) -> Self {
        Self {
            config,
            root,
            sub,
            named: HashMap::new(),
            python,
            host_fns: None,
            events: Events::default(),
            depth: 0,
            runtime: None,
        }
    }

    /// Register an additional model addressable via `llm_query(.., model=..)`.
    pub fn register_model(&mut self, name: impl Into<String>, lm: Arc<dyn LanguageModel>) {
        self.named.insert(name.into(), lm);
    }

    pub fn with_host_fns(mut self, handler: Arc<dyn SubcallHandler>) -> Self {
        self.host_fns = Some(handler);
        self
    }

    /// Access the root model, sub model, and config (for harness snapshot).
    pub fn models_and_config(&self) -> (Arc<dyn LanguageModel>, Arc<dyn LanguageModel>, RlmConfig) {
        (self.root.clone(), self.sub.clone(), self.config.clone())
    }

    pub fn with_events(mut self, sender: UnboundedSender<RlmEvent>) -> Self {
        self.events = Events::new(sender);
        self
    }

    pub fn with_runtime(mut self, runtime: Arc<AppRuntime>) -> Self {
        self.runtime = Some(runtime);
        self
    }

    pub fn at_depth(mut self, depth: usize) -> Self {
        self.depth = depth;
        self
    }

    pub(super) fn iteration_limit(&self) -> Option<usize> {
        (self.depth > 0).then_some(self.config.max_iterations)
    }

    /// Persistent multi-turn session: the REPL env and sub-call usage
    /// roll over across `send()` calls; prior exchanges are exposed to the
    /// model as `history_N` variables in the env.
    pub fn into_session(self, payload: ContextPayload) -> RlmSession {
        let handler = Arc::new(LmHandler::new(
            self.root.clone(),
            self.sub.clone(),
            self.named.clone(),
        ));
        RlmSession {
            rlm: self,
            payload,
            handler,
            run: RunState::new(),
            env: None,
            exchanges: 0,
            contexts: 1,
            transcript: Vec::new(),
            session_summary: None,
            compaction_archive: Vec::new(),
            pending_shell: Vec::new(),
        }
    }

    pub async fn completion(
        &self,
        payload: ContextPayload,
        root_prompt: Option<&str>,
        cancel: &CancellationToken,
    ) -> RlmCompletion {
        self.completion_with(payload, root_prompt, None, cancel)
            .await
    }

    pub async fn completion_with(
        &self,
        payload: ContextPayload,
        root_prompt: Option<&str>,
        turn: Option<&crate::TurnState>,
        cancel: &CancellationToken,
    ) -> RlmCompletion {
        let start = Instant::now();
        self.events.emit(RlmEvent::RunStart { depth: self.depth });
        dispatch_event(
            &self.runtime,
            LifecycleEvent {
                kind: EventKind::AgentStart,
                payload: EventPayload::Empty,
            },
        );

        // When a TurnState snapshot is provided, use it to drive the loop.
        let turn_config = turn.map(|t| &t.config);
        let effective_config = turn_config.unwrap_or(&self.config);

        if self.depth >= effective_config.max_depth {
            return self.fallback(payload, start).await;
        }

        // Use snapshot models if provided, else fall back to engine models.
        let root = turn
            .map(|t| t.root_model.clone())
            .unwrap_or_else(|| self.root.clone());
        let sub = turn
            .map(|t| t.sub_model.clone())
            .unwrap_or_else(|| self.sub.clone());
        let handler = Arc::new(LmHandler::new(root, sub, self.named.clone()));
        let run = RunState::new();
        run.begin(cancel.clone());

        let metadata = QueryMetadata::from_payload(&payload);
        let root_prompt_str = root_prompt.unwrap_or("").to_owned();
        let outcome = match self.start_env(&handler, payload, run).await {
            Ok(mut env) => {
                // Use snapshot history if provided, else build from config.
                let mut history = if let Some(turn) = turn {
                    if !turn.messages.is_empty() {
                        turn.messages.clone()
                    } else {
                        self.initial_history(metadata, root_prompt, 0)
                    }
                } else {
                    self.initial_history(metadata, root_prompt, 0)
                };
                if let Some(rt) = &self.runtime
                    && let Some(sys_msg) = history.iter_mut().find(|m| m.role == Role::System) {
                        sys_msg.content = rt
                            .filter_system_prompt(&root_prompt_str, sys_msg.content.clone())
                            .await
                            .unwrap_or_else(|_| sys_msg.content.clone());
                    }
                let outcome = self
                    .run_loop(
                        &mut env,
                        &handler,
                        &mut history,
                        1,
                        0,
                        false,
                        cancel,
                        None,
                    )
                    .await;
                env.shutdown().await;
                outcome
            }
            Err(message) => Outcome::failed(message, 0),
        };
        self.complete_run(outcome, &handler, start)
    }

    pub(super) async fn start_env(
        &self,
        handler: &Arc<LmHandler>,
        payload: ContextPayload,
        run: RunState,
    ) -> Result<Box<dyn ReplEnv>, String> {
        let subcalls = Arc::new(EngineSubcalls {
            rlm: self.clone(),
            handler: handler.clone(),
            run,
        });
        let (context, context_str) = match payload {
            ContextPayload::Str(s) => (None, Some(s)),
            ContextPayload::Json(v) => (Some(v), None),
        };
        let mut env = PythonEnvFactory { python: self.python.clone() }.create(subcalls);
        let setup = EnvSetup {
            context,
            context_str,
            setup_code: self.config.setup_code.clone(),
            cwd: self.config.cwd.clone(),
            host_fns: self.config.host_fns.clone(),
            max_concurrent_subcalls: self.config.max_concurrent_subcalls,
            max_output_chars: self.config.max_output_chars,
        };
        env.start(setup)
            .await
            .map_err(|e| format!("Error: failed to start REPL environment - {e}"))?;
        Ok(env)
    }

    pub(super) fn initial_history(
        &self,
        metadata: QueryMetadata,
        root_prompt: Option<&str>,
        _history_count: usize,
    ) -> Vec<ChatMessage> {
        let mut history = vec![
            ChatMessage {
                role: Role::System,
                content: build_system_prompt(&self.system_prompt_options()),
            },
            ChatMessage {
                role: Role::User,
                content: metadata_user_prompt(metadata, root_prompt),
            },
        ];
        if let Some(prologue) = &self.config.user_prologue {
            history.push(ChatMessage {
                role: Role::User,
                content: prologue.clone(),
            });
        }
        history
    }

    /// Build the `SystemPromptOptions` for this RLM from its config. The
    /// identity-neutral section `cyber_risk` falls back to its `DEFAULT_*`
    /// const inside `build_system_prompt`, so it is left `None` here.
    /// `env_info` is caller-supplied data threaded from `RlmConfig::env_info`.
    ///
    /// The gate is re-evaluated every build (cold root and each warm `send`),
    /// not once: `user_prologue` persists on the root config across warm turns
    /// (only `recurse` clears it for child RLMs), so it survives the whole
    /// session.
    pub(super) fn system_prompt_options(&self) -> SystemPromptOptions<'_> {
        SystemPromptOptions {
            identity: self.config.custom_system_prompt.as_deref(),
            custom_tools: self.config.tools_section.as_deref(),
            env_info: self.config.env_info.as_deref(),
            append: self.config.append_system_prompt.as_deref(),
            cwd: self.config.cwd.as_deref(),
            orchestrator: self.config.orchestrator,
            turn_cap: self.iteration_limit(),
            ..Default::default()
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn run_loop(
        &self,
        env: &mut Box<dyn ReplEnv>,
        handler: &Arc<LmHandler>,
        history: &mut Vec<ChatMessage>,
        context_count: usize,
        history_count: usize,
        warm: bool,
        cancel: &CancellationToken,
        save_point: Option<&SavePointFn>,
    ) -> Outcome {
        let start = Instant::now();
        let mut best_partial: Option<String> = None;
        let mut consecutive_errors = 0usize;
        let mut consecutive_prose = 0usize;
        let mut iterations = 0usize;

        let iteration_limit = self.iteration_limit();
        let mut i = 0usize;
        while iteration_limit.is_none_or(|limit| i < limit) {
            if cancel.is_cancelled() {
                return self.cancelled_outcome(best_partial, iterations);
            }
            if let Some(max_timeout) = self.config.max_timeout
                && start.elapsed() > max_timeout
            {
                let detail = format!(
                    "Timeout exceeded after iteration {i}: {:.1}s of {:.1}s limit",
                    start.elapsed().as_secs_f64(),
                    max_timeout.as_secs_f64()
                );
                return self.limit_outcome(LimitKind::Timeout, detail, best_partial, iterations);
            }

            history.push(ChatMessage {
                role: Role::User,
                content: build_user_prompt(i, iteration_limit, context_count, history_count, warm),
            });

            self.events.emit(RlmEvent::IterationStart {
                depth: self.depth,
                iteration: i,
            });
            dispatch_event(
                &self.runtime,
                LifecycleEvent {
                    kind: EventKind::TurnStart,
                    payload: EventPayload::TurnStart { turn: i as u32 },
                },
            );
            let iter_start = Instant::now();

            // Stream the completion and execute ```repl blocks eagerly: the
            // delta sink pushes each block into `code_tx` the moment its
            // closing fence lands, so execution overlaps the rest of the
            // stream. The iteration ends when both sides are done; outputs
            // are still only fed back via the next user message.
            let (code_tx, mut code_rx) = unbounded_channel::<Option<String>>();
            let sent = Arc::new(Mutex::new(0usize));
            let sink = self.eager_sink(i, code_tx.clone(), sent.clone());

            let stream = async {
                let result = handler
                    .complete_root(history, Some(sink), Some(self.thinking_sink(i)))
                    .await;
                if let Ok(completion) = &result {
                    // Catch-up for non-streaming models (no deltas) and any
                    // trailing block: same regex, same order, deduped by count.
                    let mut sent = sent.lock().unwrap_or_else(|e| e.into_inner());
                    for code in find_code_blocks(&completion.text).into_iter().skip(*sent) {
                        let _ = code_tx.send(Some(code));
                        *sent += 1;
                    }
                }
                let _ = code_tx.send(None);
                result
            };

            let exec = async {
                let mut blocks = Vec::new();
                while let Some(Some(code)) = code_rx.recv().await {
                    self.events.emit(RlmEvent::CodeExecStart {
                        depth: self.depth,
                        iteration: i,
                        code: code.clone(),
                    });
                    let exec_start = Instant::now();
                    let result = env
                        .exec(&code, self.config.exec_timeout)
                        .await
                        .unwrap_or_else(|e| ReplResult {
                            stdout: String::new(),
                            stderr: e.to_string(),
                            vars: Vec::new(),
                            final_answer: None,
                            duration: exec_start.elapsed(),
                            truncated: false,
                        });
                    self.events.emit(RlmEvent::CodeExecEnd {
                        depth: self.depth,
                        iteration: i,
                        stdout: result.stdout.clone(),
                        stderr: result.stderr.clone(),
                        duration: result.duration,
                    });
                    blocks.push(ExecutedBlock { code, result });
                }
                blocks
            };

            let (completion, blocks) = tokio::select! {
                pair = async { tokio::join!(stream, exec) } => pair,
                () = cancel.cancelled() => {
                    return self.cancelled_outcome(best_partial, iterations);
                }
            };
            let response = match completion {
                Ok(completion) => completion.text,
                Err(e) => {
                    let message = format!("Error: root completion failed - {e}");
                    return match best_partial {
                        Some(partial) => Outcome {
                            stop: StopCause::Failed,
                            response: partial,
                            error: Some(message),
                            iterations,
                        },
                        None => Outcome::failed(message, iterations),
                    };
                }
            };
            iterations = i + 1;
            self.events.emit(RlmEvent::RootText {
                depth: self.depth,
                iteration: i,
                text: response.clone(),
            });

            // Guardrails, in upstream order: consecutive errors, budget, tokens.
            let had_error = blocks.iter().any(|b| !b.result.stderr.is_empty());
            if had_error {
                consecutive_errors += 1;
            } else {
                consecutive_errors = 0;
            }
            if let Some(max_errors) = self.config.max_errors
                && consecutive_errors >= max_errors
            {
                let detail = format!(
                    "Error threshold exceeded: {consecutive_errors} consecutive errors (limit: {max_errors})"
                );
                if !response.trim().is_empty() {
                    best_partial = Some(response);
                }
                return self.limit_outcome(LimitKind::Errors, detail, best_partial, iterations);
            }
            let usage = handler.usage();
            if let Some(max_budget) = self.config.max_budget
                && usage.cost.total > max_budget
            {
                let detail = format!(
                    "Budget exceeded after iteration {}: spent ${:.6} of ${:.6} budget",
                    i + 1,
                    usage.cost.total,
                    max_budget
                );
                if !response.trim().is_empty() {
                    best_partial = Some(response);
                }
                return self.limit_outcome(LimitKind::Budget, detail, best_partial, iterations);
            }
            if let Some(max_tokens) = self.config.max_tokens
                && usage.input + usage.output > max_tokens
            {
                let detail = format!(
                    "Token limit exceeded after iteration {}: {} of {} tokens",
                    i + 1,
                    usage.input + usage.output,
                    max_tokens
                );
                if !response.trim().is_empty() {
                    best_partial = Some(response);
                }
                return self.limit_outcome(LimitKind::Tokens, detail, best_partial, iterations);
            }

            let final_answer = blocks.iter().find_map(|b| b.result.final_answer.clone());
            if !response.trim().is_empty() {
                best_partial = Some(response.clone());
            }
            if let Some(answer) = final_answer {
                self.events.emit(RlmEvent::AnswerSubmitted {
                    depth: self.depth,
                    content: answer.clone(),
                });
                dispatch_event(
                    &self.runtime,
                    LifecycleEvent {
                        kind: EventKind::TurnEnd,
                        payload: EventPayload::TurnEnd {
                            turn: i as u32,
                            had_code: !blocks.is_empty(),
                        },
                    },
                );
                self.events.emit(RlmEvent::IterationEnd {
                    depth: self.depth,
                    iteration: i,
                    duration: iter_start.elapsed(),
                });
                return Outcome {
                    stop: StopCause::Answer,
                    response: answer,
                    error: None,
                    iterations,
                };
            }

            // Prose-bounce: a block-free turn executes nothing. The first
            // gets a corrective `PROSE_NUDGE` reminder so the model can
            // either submit its prose as the answer or get back to work. A
            // second consecutive block-free turn is accepted as the final
            // answer (`StopCause::Prose`), matching `DEFAULT_MECHANICS`.
            let had_code = !find_code_blocks(&response).is_empty();
            if had_code {
                consecutive_prose = 0;
            } else {
                consecutive_prose += 1;
                if consecutive_prose >= 2 {
                    let prose = response.trim().to_owned();
                    self.events.emit(RlmEvent::AnswerSubmitted {
                        depth: self.depth,
                        content: prose.clone(),
                    });
                    dispatch_event(
                        &self.runtime,
                        LifecycleEvent {
                            kind: EventKind::TurnEnd,
                            payload: EventPayload::TurnEnd {
                                turn: i as u32,
                                had_code: false,
                            },
                        },
                    );
                    self.events.emit(RlmEvent::IterationEnd {
                        depth: self.depth,
                        iteration: i,
                        duration: iter_start.elapsed(),
                    });
                    return Outcome {
                        stop: StopCause::Prose,
                        response: prose,
                        error: None,
                        iterations,
                    };
                }
                // First block-free turn: emit the turn-end, push the
                // assistant prose (so the model sees its own last message
                // is the thing being bounced), then the nudge as a user
                // message. Continue to the next iteration.
                dispatch_event(
                    &self.runtime,
                    LifecycleEvent {
                        kind: EventKind::TurnEnd,
                        payload: EventPayload::TurnEnd {
                            turn: i as u32,
                            had_code: false,
                        },
                    },
                );
                self.events.emit(RlmEvent::IterationEnd {
                    depth: self.depth,
                    iteration: i,
                    duration: iter_start.elapsed(),
                });
                history.push(ChatMessage {
                    role: Role::Assistant,
                    content: response,
                });
                history.push(ChatMessage {
                    role: Role::User,
                    content: PROSE_NUDGE.to_owned(),
                });
                i += 1;
                continue;
            }

            // Bank the assistant turn up to the last closing fence: trailing
            // prose after the final block is dead weight in history (the
            // streaming sink already aborts there; this covers non-streamed
            // completions).
            let banked = match last_block_end(&response) {
                Some(end) => response[..end].to_owned(),
                None => response,
            };
            history.push(ChatMessage {
                role: Role::Assistant,
                content: banked,
            });
            if let Some(outputs) = format_iteration(&blocks, self.config.max_output_chars) {
                history.push(ChatMessage {
                    role: Role::User,
                    content: outputs,
                });
            }
            dispatch_event(
                &self.runtime,
                LifecycleEvent {
                    kind: EventKind::TurnEnd,
                    payload: EventPayload::TurnEnd {
                        turn: i as u32,
                        had_code: !blocks.is_empty(),
                    },
                },
            );
            self.events.emit(RlmEvent::IterationEnd {
                depth: self.depth,
                iteration: i,
                duration: iter_start.elapsed(),
            });

            // SAVE POINT: flush pending writes and drain queues (steer,
            // follow-up) so they land in the next iteration's history.
            if let Some(sp) = save_point {
                let queued = sp(i);
                if !queued.is_empty() {
                    history.push(ChatMessage {
                        role: Role::User,
                        content: queued
                            .iter()
                            .map(|m| m.content.clone())
                            .collect::<Vec<_>>()
                            .join(
                                "

",
                            ),
                    });
                }
            }

            i += 1;
        }

        let Some(_) = iteration_limit else {
            return Outcome::failed(
                "Error: root RLM loop exited unexpectedly despite having no turn limit".to_owned(),
                iterations,
            );
        };

        // Iterations exhausted: synthesize a final answer from the trajectory.
        history.push(ChatMessage {
            role: Role::Assistant,
            content: DEFAULT_ANSWER_NUDGE.to_owned(),
        });
        match handler
            .complete_root(
                history,
                Some(self.delta_sink(iterations)),
                Some(self.thinking_sink(iterations)),
            )
            .await
        {
            Ok(completion) => {
                self.events.emit(RlmEvent::RootText {
                    depth: self.depth,
                    iteration: iterations,
                    text: completion.text.clone(),
                });
                Outcome {
                    stop: StopCause::DefaultAnswer,
                    response: completion.text,
                    error: None,
                    iterations,
                }
            }
            Err(e) => {
                let message = format!("Error: default answer completion failed - {e}");
                match best_partial {
                    Some(partial) => Outcome {
                        stop: StopCause::Failed,
                        response: partial,
                        error: Some(message),
                        iterations,
                    },
                    None => Outcome::failed(message, iterations),
                }
            }
        }
    }

    fn delta_sink(&self, iteration: usize) -> DeltaSink {
        let events = self.events.clone();
        let depth = self.depth;
        Arc::new(move |delta: &str| {
            events.emit(RlmEvent::RootDelta {
                depth,
                iteration,
                delta: delta.to_owned(),
            });
            true
        })
    }

    fn thinking_sink(&self, iteration: usize) -> ThinkingSink {
        let events = self.events.clone();
        let depth = self.depth;
        Arc::new(move |delta: &str| {
            events.emit(RlmEvent::RootThinkingDelta {
                depth,
                iteration,
                delta: delta.to_owned(),
            });
        })
    }

    /// Delta sink that, besides emitting `RootDelta`, watches the
    /// accumulating text for completed ```repl fences and pushes each new
    /// block into `code_tx` for immediate execution. `sent` counts blocks
    /// already pushed so the post-completion catch-up can dedupe.
    ///
    /// Implements the turn boundary: once at least one block is complete,
    /// only whitespace or the opening of another ```repl fence may follow.
    /// Anything else is prose written after a "tool call" — the sink returns
    /// `false` to abort generation, and the loop feeds the block outputs
    /// back immediately instead.
    fn eager_sink(
        &self,
        iteration: usize,
        code_tx: UnboundedSender<Option<String>>,
        sent: Arc<Mutex<usize>>,
    ) -> DeltaSink {
        let events = self.events.clone();
        let depth = self.depth;
        let buffer = Mutex::new(String::new());
        Arc::new(move |delta: &str| {
            events.emit(RlmEvent::RootDelta {
                depth,
                iteration,
                delta: delta.to_owned(),
            });
            let mut buffer = buffer.lock().unwrap_or_else(|e| e.into_inner());
            buffer.push_str(delta);
            let blocks = find_code_blocks(&buffer);
            let mut sent = sent.lock().unwrap_or_else(|e| e.into_inner());
            for code in blocks.into_iter().skip(*sent) {
                let _ = code_tx.send(Some(code));
                *sent += 1;
            }
            match last_block_end(&buffer) {
                Some(end) => {
                    let tail = buffer[end..].trim_start();
                    tail.is_empty() || could_open_block(tail)
                }
                None => true,
            }
        })
    }

    /// `depth >= max_depth`: the RLM is just an LM.
    pub(super) async fn fallback(&self, payload: ContextPayload, start: Instant) -> RlmCompletion {
        let content = match payload {
            ContextPayload::Str(s) => s,
            ContextPayload::Json(Value::String(s)) => s,
            ContextPayload::Json(v) => v.to_string(),
        };
        let messages = [ChatMessage {
            role: Role::User,
            content,
        }];
        let (response, usage, usage_by_model, stop, error) =
            match self.root.complete(&messages).await {
                Ok(completion) => {
                    let usage_by_model =
                        usage_breakdowns_for_model(self.root.name(), completion.usage);
                    (
                        completion.text,
                        completion.usage,
                        usage_by_model,
                        StopCause::Fallback,
                        None,
                    )
                }
                Err(e) => (
                    format!("Error: LM query failed at max depth - {e}"),
                    Usage::default(),
                    Vec::new(),
                    StopCause::Failed,
                    Some(e),
                ),
            };
        let duration = start.elapsed();
        self.events.emit(RlmEvent::RunEnd {
            depth: self.depth,
            stop,
            iterations: 0,
            duration,
        });
        RlmCompletion {
            response,
            usage,
            usage_by_model,
            iterations: 0,
            duration,
            stop,
            error,
        }
    }

    fn complete_run(&self, outcome: Outcome, handler: &LmHandler, start: Instant) -> RlmCompletion {
        let duration = start.elapsed();
        dispatch_event(
            &self.runtime,
            LifecycleEvent {
                kind: EventKind::AgentEnd,
                payload: EventPayload::AgentEnd {
                    stop_cause: format!("{:?}", outcome.stop),
                },
            },
        );
        self.events.emit(RlmEvent::RunEnd {
            depth: self.depth,
            stop: outcome.stop,
            iterations: outcome.iterations,
            duration,
        });
        RlmCompletion {
            response: outcome.response,
            usage: handler.usage(),
            usage_by_model: handler.usage_by_model(),
            iterations: outcome.iterations,
            duration,
            stop: outcome.stop,
            error: outcome.error,
        }
    }

    pub(super) fn complete_run_since(
        &self,
        outcome: Outcome,
        handler: &LmHandler,
        usage_before: &Usage,
        usage_by_model_before: &[UsageBreakdown],
        start: Instant,
    ) -> RlmCompletion {
        let duration = start.elapsed();
        dispatch_event(
            &self.runtime,
            LifecycleEvent {
                kind: EventKind::AgentEnd,
                payload: EventPayload::AgentEnd {
                    stop_cause: format!("{:?}", outcome.stop),
                },
            },
        );
        self.events.emit(RlmEvent::RunEnd {
            depth: self.depth,
            stop: outcome.stop,
            iterations: outcome.iterations,
            duration,
        });
        RlmCompletion {
            response: outcome.response,
            usage: handler.usage_since(usage_before),
            usage_by_model: handler.usage_by_model_since(usage_by_model_before),
            iterations: outcome.iterations,
            duration,
            stop: outcome.stop,
            error: outcome.error,
        }
    }

    fn limit_outcome(
        &self,
        kind: LimitKind,
        detail: String,
        best_partial: Option<String>,
        iterations: usize,
    ) -> Outcome {
        self.events.emit(RlmEvent::LimitHit {
            depth: self.depth,
            limit: kind,
            detail: detail.clone(),
        });
        Outcome {
            stop: StopCause::Limit(kind),
            response: best_partial.unwrap_or_else(|| format!("Error: {detail}")),
            error: Some(detail),
            iterations,
        }
    }

    fn cancelled_outcome(&self, best_partial: Option<String>, iterations: usize) -> Outcome {
        Outcome {
            stop: StopCause::Cancelled,
            response: best_partial.unwrap_or_else(|| "Execution cancelled".to_owned()),
            error: Some("Execution cancelled".to_owned()),
            iterations,
        }
    }
}

