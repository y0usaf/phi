//! Rlm struct + impl: the main engine loop.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use phi_ai::{ToolDefinition, Usage};
use phi_repl::{EnvSetup, ReplEnv, ReplResult, SubcallHandler};
use serde_json::{Value, json};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio_util::sync::CancellationToken;

use phi_ext::{EventKind, EventPayload, LifecycleEvent};
use phi_runtime::AppRuntime;

use crate::lm_core::{DeltaSink, LanguageModel, LmHandler, ThinkingSink, ToolCallEvent, ToolCallSink, UsageBreakdown};
use crate::llm_parse::{ExecutedBlock, format_execution_result};

/// A tool call result arriving from the streaming sink, carrying the code
/// extracted from the tool call's arguments for eager execution.
struct ToolCallResult {
    code: Option<String>,
}
use crate::prompts::{DEFAULT_ANSWER_NUDGE, QueryMetadata, SystemPromptOptions, build_system_prompt, build_user_prompt, metadata_user_prompt};
use std::sync::LazyLock;
use regex_lite::Regex;

static TASK_COMPLETE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)<task_complete\s+output="(?P<output>.*?)"\s*/>"#)
        .expect("task_complete regex")
});

/// Maximum consecutive block-free turns (no ```repl``` block, no
/// `<task_complete>` token) before the prose-safety-valve fires. High enough
/// that a verbally-planning model won't trip it, low enough to catch a
/// genuinely stuck model before exhausting `max_iterations`.
const MAX_CONSECUTIVE_PROSE: usize = 4;

/// The native tool definition for the `repl` tool. The model calls this
/// instead of writing ```repl fences; the `code` argument is executed in the
/// Python REPL and the result is returned as a tool result.
fn repl_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: "repl".to_owned(),
        description: "Execute Python code in the REPL. The REPL persists across calls. Use print(...) to see output. Set answer['content'] and answer['ready'] = True to submit a final answer.".to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "code": {
                    "type": "string",
                    "description": "Python code to execute in the REPL."
                }
            },
            "required": ["code"]
        }),
    }
}

use crate::lm_core::{ChatMessage, ContextPayload, Role};

use crate::config::{EnvFactory, Events, LimitKind, PythonEnvFactory, RlmCompletion, RlmConfig, RlmEvent, SavePointFn, StopCause};
use super::helpers::{dispatch_event, usage_breakdowns_for_model};
use super::state::RunState;
use super::TurnState;
use super::subcalls::{EngineSubcalls, Outcome};

#[derive(Clone)]
pub struct Rlm {
    pub config: RlmConfig,
    pub root: Arc<dyn LanguageModel>,
    pub sub: Arc<dyn LanguageModel>,
    pub named: HashMap<String, Arc<dyn LanguageModel>>,
    pub python: String,
    pub host_fns: Option<Arc<dyn SubcallHandler>>,
    pub events: Events,
    pub depth: usize,
    pub runtime: Option<Arc<AppRuntime>>,
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

    /// Register the `repl` tool definition on the root model, enabling
    /// native tool-call-based REPL execution instead of text-based fences.
    pub fn register_repl_tool(&self) {
        self.root.set_tools(vec![repl_tool_definition()]);
    }

    pub fn at_depth(mut self, depth: usize) -> Self {
        self.depth = depth;
        self
    }

    pub fn iteration_limit(&self) -> Option<usize> {
        (self.depth > 0).then_some(self.config.max_iterations)
    }

    /// Build the shared LmHandler for this Rlm instance.
    pub fn handler(&self) -> Arc<LmHandler> {
        Arc::new(LmHandler::new(
            self.root.clone(),
            self.sub.clone(),
            self.named.clone(),
        ))
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
        turn: Option<&TurnState>,
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
            return self.fallback(payload, start, cancel).await;
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
                        self.initial_history(metadata, root_prompt)
                    }
                } else {
                    self.initial_history(metadata, root_prompt)
                };
                if let Some(rt) = &self.runtime
                    && let Some(sys_msg) = history.iter_mut().find(|m| m.role == Role::System) {
                        sys_msg.content = rt
                            .filter_system_prompt(&root_prompt_str, sys_msg.content.clone())
                            .await
                            .unwrap_or_else(|_| sys_msg.content.clone());
                    }
                self.register_repl_tool();
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

    pub async fn start_env(
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

    pub fn initial_history(
        &self,
        metadata: QueryMetadata,
        root_prompt: Option<&str>,
    ) -> Vec<ChatMessage> {
        let mut history = vec![
            ChatMessage {
                role: Role::System,
                content: build_system_prompt(&self.system_prompt_options()),
            ..Default::default()
            },
            ChatMessage {
                role: Role::User,
                content: metadata_user_prompt(metadata, root_prompt),
            ..Default::default()
            },
        ];
        if let Some(prologue) = &self.config.user_prologue {
            history.push(ChatMessage {
                role: Role::User,
                content: prologue.clone(),
            ..Default::default()
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
    pub fn system_prompt_options(&self) -> SystemPromptOptions<'_> {
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
    pub async fn run_loop(
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
                ..Default::default()
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

            // Channel for eagerly executing tool calls during the stream.
            // The sender is cloned into the tool_sink closure; we drop our
            // copy so the channel closes when the closure is dropped (after
            // the stream completes), unblocking the exec receiver.
            let (tool_tx, mut tool_rx) = unbounded_channel::<ToolCallResult>();
            let tool_sink = self.tool_call_sink(i, tool_tx.clone());
            drop(tool_tx);

            let stream = async {
                handler
                    .complete_root(
                        history,
                        Some(self.delta_sink(i)),
                        Some(self.thinking_sink(i)),
                        Some(tool_sink),
                        cancel,
                    )
                    .await
            };

            // Eagerly execute tool calls as they arrive during the stream.
            let exec = async {
                let mut blocks = Vec::new();
                while let Some(result) = tool_rx.recv().await {
                    if let Some(code) = result.code {
                        self.events.emit(RlmEvent::CodeExecStart {
                            depth: self.depth,
                            iteration: i,
                            code: code.clone(),
                        });
                        let exec_start = Instant::now();
                        let repl_result = env
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
                            stdout: repl_result.stdout.clone(),
                            stderr: repl_result.stderr.clone(),
                            duration: repl_result.duration,
                        });
                        blocks.push(ExecutedBlock {
                            code,
                            result: repl_result,
                        });
                    }
                }
                blocks
            };

            let (completion, blocks) = tokio::select! {
                pair = async { tokio::join!(stream, exec) } => pair,
                () = cancel.cancelled() => {
                    return self.cancelled_outcome(best_partial, iterations);
                }
            };
            let completion = match completion {
                Ok(completion) => completion,
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
            let response = completion.text;
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

            let had_tool_calls = !completion.tool_calls.is_empty() && !blocks.is_empty();

            if had_tool_calls {
                consecutive_prose = 0;

                // Bank the assistant message with its tool calls.
                let tool_calls: Vec<crate::lm_core::ToolCall> = completion
                    .tool_calls
                    .iter()
                    .map(|tc| crate::lm_core::ToolCall {
                        id: tc.id.clone(),
                        name: tc.name.clone(),
                        arguments: tc.arguments.clone(),
                    })
                    .collect();
                history.push(ChatMessage::assistant_with_tools(response, tool_calls));

                // Feed each tool result back as a Role::Tool message.
                for block in &blocks {
                    let tool_call_id = completion
                        .tool_calls
                        .iter()
                        .zip(blocks.iter())
                        .find_map(|(tc, b)| {
                            if b.code == block.code {
                                Some(tc.id.clone())
                            } else {
                                None
                            }
                        })
                        .unwrap_or_default();
                    let output = format_execution_result(&block.result);
                    history.push(ChatMessage::tool_result(tool_call_id, output));
                }

                dispatch_event(
                    &self.runtime,
                    LifecycleEvent {
                        kind: EventKind::TurnEnd,
                        payload: EventPayload::TurnEnd {
                            turn: i as u32,
                            had_code: true,
                        },
                    },
                );
                self.events.emit(RlmEvent::IterationEnd {
                    depth: self.depth,
                    iteration: i,
                    duration: iter_start.elapsed(),
                });

                // Save Point: flush pending writes and drain queues.
                if let Some(sp) = save_point {
                    let queued = sp(i);
                    if !queued.is_empty() {
                        history.push(ChatMessage {
                            role: Role::User,
                            content: queued
                                .iter()
                                .map(|m| m.content.clone())
                                .collect::<Vec<_>>()
                                .join("\n\n"),
                            ..Default::default()
                        });
                    }
                }

                i += 1;
                continue;
            }

            // No tool calls: the model emitted pure text (prose).
            // Check for explicit <task_complete> token (fallback for models
            // that don't support native tool calls).
            if let Some(caps) = TASK_COMPLETE_RE.captures(&response) {
                let final_answer = caps
                    .name("output")
                    .map(|m| m.as_str().to_owned())
                    .unwrap_or_else(|| response.trim().to_owned());
                self.events.emit(RlmEvent::AnswerSubmitted {
                    depth: self.depth,
                    content: final_answer.clone(),
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
                    response: final_answer,
                    error: None,
                    iterations,
                };
            }

            // Safety valve: after MAX_CONSECUTIVE_PROSE block-free turns,
            // accept the prose as the final answer.
            consecutive_prose += 1;
            if consecutive_prose >= MAX_CONSECUTIVE_PROSE {
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

            // Bank the prose silently and continue.
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
                ..Default::default()
            });
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
            ..Default::default()
        });
        match handler
            .complete_root(
                history,
                Some(self.delta_sink(iterations)),
                Some(self.thinking_sink(iterations)),
                None,
                cancel,
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

    /// Tool-call sink that executes code eagerly during the stream. When a
    /// `ToolCallEnd` event fires, the `code` argument is extracted from the
    /// tool call's arguments and pushed into `tool_tx` for immediate execution
    /// by the `exec` task in `run_loop`.
    fn tool_call_sink(
        &self,
        _iteration: usize,
        tool_tx: UnboundedSender<ToolCallResult>,
    ) -> ToolCallSink {
        let tx = tool_tx.clone();
        Arc::new(move |event: ToolCallEvent| {
            if let ToolCallEvent::End { arguments, .. } = &event {
                let code = arguments
                    .get("code")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_owned());
                let _ = tx.send(ToolCallResult { code });
            }
        })
    }

    /// `depth >= max_depth`: the RLM is just an LM.
    pub async fn fallback(&self, payload: ContextPayload, start: Instant, cancel: &CancellationToken) -> RlmCompletion {
        let content = match payload {
            ContextPayload::Str(s) => s,
            ContextPayload::Json(Value::String(s)) => s,
            ContextPayload::Json(v) => v.to_string(),
        };
        let messages = [ChatMessage {
            role: Role::User,
            content,
        ..Default::default()
        }];
        let (response, usage, usage_by_model, stop, error) =
            match self.root.complete(&messages, cancel).await {
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

    pub fn complete_run_since(
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

