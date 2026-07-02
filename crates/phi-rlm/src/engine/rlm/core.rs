//! Rlm struct, lifecycle methods, and main run_loop.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;
use std::sync::LazyLock;
use std::time::Instant;

use phi_ai::ToolDefinition;
use phi_repl::{EnvSetup, ReplEnv, ReplResult, SubcallHandler};
use serde_json::json;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio_util::sync::CancellationToken;

use phi_ext::AppRuntime;
use phi_ext::{EventKind, EventPayload, EventReturn, LifecycleEvent};

use crate::config::{
    EnvFactory, Events, LimitKind, ProsePolicy, PythonEnvFactory, RlmCompletion, RlmConfig,
    RlmEvent, SavePointFn, StopCause,
};
use crate::llm_parse::{ExecutedBlock, format_execution_result};
use crate::lm_core::{ChatMessage, ContextPayload, LanguageModel, LmHandler, Role};
use crate::prompts::{
    DEFAULT_ANSWER_NUDGE, PROSE_NUDGE, QueryMetadata, STRICT_PROSE_NUDGE, SystemPromptOptions,
    build_system_prompt, build_user_prompt, metadata_user_prompt,
};

use super::super::TurnState;
use super::super::helpers::{dispatch_await, dispatch_event};
use super::super::state::RunState;
use super::super::subcalls::{EngineSubcalls, Outcome};

use regex_lite::Regex;

static TASK_COMPLETE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)<task_complete\s+output="(?P<output>.*?)"\s*/>"#)
        .expect("task_complete regex")
});

/// Number of consecutive block-free turns after which a `Role::User` nudge
/// is injected, reminding the model to use the REPL or submit a final answer.
/// Low enough to course-correct a model that is drifting in prose, high enough
/// that a single turn of verbal planning is not interrupted.
const PROSE_NUDGE_THRESHOLD: usize = 2;

/// Maximum consecutive block-free turns (no ```repl``` block, no
/// `<task_complete>` token) before the prose-safety-valve fires and the prose
/// is accepted as the final answer. High enough that a verbally-planning model
/// won't trip it (with the nudge at `PROSE_NUDGE_THRESHOLD` giving it a chance
/// to course-correct first), low enough to catch a genuinely stuck model
/// before exhausting `max_iterations`.
const MAX_CONSECUTIVE_PROSE: usize = 5;

/// Last-resort ceiling for `ProsePolicy::Strict`: the prose valve is disabled,
/// but an uncapped root loop must not spin forever on pure prose. After this
/// many consecutive block-free turns the run fails rather than silently
/// accepting prose as an answer.
const MAX_CONSECUTIVE_PROSE_STRICT: usize = 12;

/// Number of additional identical-signature iterations (after the first match)
/// that must accumulate before a corrective nudge is injected into history.
/// The counter starts at 0 on the first match, so the nudge fires after
/// `LOOP_NUDGE_THRESHOLD + 1` total identical iterations (e.g., value 2 means
/// 3 identical iterations before the nudge). The signature includes stdout and
/// stderr so that polling code whose output changes (e.g. job status checks
/// returning new runtimes) is never flagged as looping.
const LOOP_NUDGE_THRESHOLD: usize = 2;

/// Maximum value of the additional-identical-iterations counter before the
/// engine stops with `LimitKind::Loop`. Because the counter starts at 0 on the
/// first match, this fires after `MAX_CONSECUTIVE_LOOP + 1` total identical
/// iterations (e.g., value 5 means 6 identical iterations before the hard
/// stop). The nudge fires at `LOOP_NUDGE_THRESHOLD` to give the model a chance
/// to recover first.
const MAX_CONSECUTIVE_LOOP: usize = 5;

/// Nudge injected into history after `LOOP_NUDGE_THRESHOLD` identical
/// iterations, asking the model to re-read its output and try something new.
const LOOP_NUDGE: &str = "Your recent iterations executed identical code producing identical output — no progress is being made. Step back: re-read the actual output, state a new hypothesis in prose, and take a different action (different command, different file, or delegate a fresh-context look via subagent(...)).";

/// A tool call result arriving from the streaming sink, carrying the provider-
/// assigned tool call id and the code extracted from the tool call's arguments
/// for eager execution. The id is threaded into `ExecutedBlock` so the exec
/// loop can pair each result with the correct `tool_result` message without
/// relying on code-string equality.
pub struct ToolCallResult {
    pub id: String,
    pub code: Option<String>,
}

/// The native tool definition for the `repl` tool. The model calls this
/// instead of writing ```repl fences; the `code` argument is executed in the
/// Python REPL and the result is returned as a tool result.
pub fn repl_tool_definition() -> ToolDefinition {
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

    pub(crate) async fn completion_with(
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

        if self.depth >= effective_config.max_rlm_depth {
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
                    && let Some(sys_msg) = history.iter_mut().find(|m| m.role == Role::System)
                {
                    // Race the system-prompt filter against cancellation (mirror
                    // of the root-session path). Bind `original` first so the
                    // future and cancel fallback own clones, isolating the
                    // mutable borrow of `sys_msg` to the final assignment.
                    let original = sys_msg.content.clone();
                    let filtered = tokio::select! {
                        r = rt.filter_system_prompt(&root_prompt_str, original.clone()) => {
                            r.unwrap_or_else(|_| original.clone())
                        }
                        () = cancel.cancelled() => original.clone(),
                    };
                    sys_msg.content = filtered;
                }
                self.register_repl_tool();
                let outcome = self
                    .run_loop(&mut env, &handler, &mut history, 1, 0, false, cancel, None)
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
        let mut env = PythonEnvFactory {
            python: self.python.clone(),
        }
        .create(subcalls);
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

    /// Build the `SystemPromptOptions` for this RLM from its config.
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
            strict_tools: self.config.prose_policy == ProsePolicy::Strict,
            turn_cap: self.iteration_limit(),
            skills: self.config.skills.clone(),
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
        // Loop-detection state: tracks the hash of all (code, stdout, stderr)
        // triples from the most recent iteration. Reset whenever the signature
        // changes or an empty-blocks iteration occurs.
        let mut prev_loop_sig: Option<u64> = None;
        let mut consecutive_loop = 0usize;
        // Nudge is injected at most once per contiguous stall sequence; reset
        // when the stall breaks (prev_loop_sig changes) so a genuinely new
        // repeated pattern can still receive a nudge.
        let mut loop_nudge_injected = false;

        let iteration_limit = self.iteration_limit();
        let mut i = 0usize;
        // Steer text queued by save_point in the previous tool-call turn. Carried
        // forward and prepended to the next turn's user message rather than pushed
        // as a separate Role::User object immediately after tool_result messages.
        // Merging steer text with the next build_user_prompt (rather than injecting
        // a separate user object) avoids a consecutive-user-message pair that could
        // cause providers to merge or reject the interleaved tool_result + text turn.
        let mut pending_steer: Option<String> = None;
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

            // Prepend any steer text carried from the previous turn's save_point
            // so the model sees it at the top of the next turn's user prompt.
            let turn_prompt =
                build_user_prompt(i, iteration_limit, context_count, history_count, warm);
            let turn_prompt = if let Some(steer) = pending_steer.take() {
                format!("{steer}\n\n{turn_prompt}")
            } else {
                turn_prompt
            };
            history.push(ChatMessage {
                role: Role::User,
                content: turn_prompt,
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

            // ── Context injection from extensions ───────────────────────────
            // Extensions can inject (label, content) pairs that get prepended
            // to the user prompt for this turn. This mirrors pi's `context`
            // event with `MessageInjection` returns.
            if let Some(rt) = &self.runtime {
                // Race context injection against cancellation. On cancel,
                // yield no injections and let the outer `select!` catch cancel
                // on the next poll — a blocked Context handler can't strand the
                // iteration.
                let injections = tokio::select! {
                    r = rt.context_injections(i as u32) => r.unwrap_or_default(),
                    () = cancel.cancelled() => Vec::new(),
                };
                if !injections.is_empty() {
                    let mut combined = String::new();
                    for (label, content) in &injections {
                        combined.push_str(&format!(
                            "# {label}
{content}

"
                        ));
                    }
                    if let Some(last) = history.last_mut() {
                        if last.role == Role::User {
                            last.content = format!("{combined}{}", last.content);
                        }
                    }
                }
            }

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
                    let tool_call_id = result.id;
                    if let Some(code) = result.code {
                        // ── ToolCall interception ──────────────────────────
                        // Dispatch tool_call event before execution. If any
                        // handler returns ToolBlock, skip execution and inject
                        // a blocked result instead.
                        let returns = dispatch_await(
                            &self.runtime,
                            LifecycleEvent {
                                kind: EventKind::ToolCall,
                                payload: EventPayload::ToolCall {
                                    name: "repl".to_owned(),
                                    args: json!({ "code": code }),
                                },
                            },
                        )
                        .await;

                        let blocked = returns
                            .iter()
                            .any(|r| matches!(r, EventReturn::ToolBlock { .. }));

                        self.events.emit(RlmEvent::CodeExecStart {
                            depth: self.depth,
                            iteration: i,
                            code: code.clone(),
                        });
                        let exec_start = Instant::now();

                        // ── Extension: ToolExecutionStart ──────────────────────
                        if let Some(rt) = &self.runtime {
                            let call_id = format!("d{}i{}", self.depth, i);
                            let _ = rt
                                .on_tool_execution_start(&call_id, "repl", json!({ "code": code }))
                                .await;
                        }

                        let repl_result = if blocked {
                            // Find the block reason for the error message.
                            let reason = returns
                                .iter()
                                .find_map(|r| {
                                    if let EventReturn::ToolBlock { reason } = r {
                                        Some(reason.clone())
                                    } else {
                                        None
                                    }
                                })
                                .unwrap_or_else(|| "Blocked by extension".to_owned());
                            ReplResult {
                                stdout: String::new(),
                                stderr: format!("Tool blocked: {reason}"),
                                vars: Vec::new(),
                                final_answer: None,
                                duration: exec_start.elapsed(),
                                truncated: false,
                            }
                        } else {
                            env.exec(&code, self.config.exec_timeout)
                                .await
                                .unwrap_or_else(|e| ReplResult {
                                    stdout: String::new(),
                                    stderr: e.to_string(),
                                    vars: Vec::new(),
                                    final_answer: None,
                                    duration: exec_start.elapsed(),
                                    truncated: false,
                                })
                        };

                        // ── ToolResult interception ───────────────────────
                        // Dispatch tool_result event after execution. If any
                        // handler returns ToolResultOverride, use the override
                        // content instead of the actual result.
                        // ── Extension: ToolExecutionEnd ────────────────────────
                        if let Some(rt) = &self.runtime {
                            let call_id = format!("d{}i{}", self.depth, i);
                            let _ = rt
                                .on_tool_execution_end(
                                    &call_id,
                                    "repl",
                                    serde_json::json!({
                                        "stdout": repl_result.stdout,
                                        "stderr": repl_result.stderr,
                                        "duration": repl_result.duration.as_secs_f64(),
                                    }),
                                    !repl_result.stderr.is_empty(),
                                )
                                .await;
                        }

                        let result_str = format_execution_result(&repl_result);
                        let returns = dispatch_await(
                            &self.runtime,
                            LifecycleEvent {
                                kind: EventKind::ToolResult,
                                payload: EventPayload::ToolResult {
                                    name: "repl".to_owned(),
                                    result: result_str.clone(),
                                    error: if repl_result.stderr.is_empty() {
                                        None
                                    } else {
                                        Some(repl_result.stderr.clone())
                                    },
                                },
                            },
                        )
                        .await;

                        let mut final_result = repl_result;
                        for r in &returns {
                            if let EventReturn::ToolResultOverride {
                                content,
                                is_error,
                                details: _,
                            } = r
                            {
                                final_result.stdout = content.clone();
                                if is_error.unwrap_or(false) {
                                    final_result.stderr = content.clone();
                                    final_result.stdout = String::new();
                                } else {
                                    final_result.stderr = String::new();
                                }
                            }
                        }

                        self.events.emit(RlmEvent::CodeExecEnd {
                            depth: self.depth,
                            iteration: i,
                            stdout: final_result.stdout.clone(),
                            stderr: final_result.stderr.clone(),
                            duration: final_result.duration,
                        });
                        blocks.push(ExecutedBlock {
                            id: tool_call_id,
                            code,
                            result: final_result,
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
            } else if !blocks.is_empty() {
                // Only reset on a turn with actual clean code execution; prose turns
                // (blocks empty) are neutral and do not count as success — a model
                // that interleaves prose between failing code turns should still
                // accumulate toward the max_errors limit.
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

            // A turn has tool calls when the provider emitted at least one tool_use
            // block, regardless of whether we extracted `code` from every call.
            // The earlier check (`&& !blocks.is_empty()`) caused two bugs:
            // (a) when some calls lacked `code`, N tool_uses landed in the assistant
            //     message but only M < N tool_results were pushed → HTTP 400 on
            //     providers that require a 1:1 correspondence;
            // (b) when ALL calls lacked `code`, the entire assistant turn was silently
            //     treated as prose and tool calls were dropped from history entirely.
            let had_tool_calls = !completion.tool_calls.is_empty();

            if had_tool_calls {
                consecutive_prose = 0;

                // ── Loop detection ─────────────────────────────────────────
                // Hash all (code, stdout, stderr) triples from this iteration.
                // Output is included in the signature so that polling code
                // whose output legitimately changes (e.g. job status checks)
                // is never falsely flagged as a stall.
                // When no blocks executed (all calls lacked `code`), skip loop
                // detection for this turn — an empty hash would trivially match
                // and falsely accumulate the loop counter.
                if !blocks.is_empty() {
                    let loop_sig = {
                        let mut h = DefaultHasher::new();
                        for b in &blocks {
                            b.code.hash(&mut h);
                            b.result.stdout.hash(&mut h);
                            b.result.stderr.hash(&mut h);
                        }
                        h.finish()
                    };
                    if prev_loop_sig == Some(loop_sig) {
                        consecutive_loop += 1;
                    } else {
                        consecutive_loop = 0;
                        prev_loop_sig = Some(loop_sig);
                        // New stall pattern: allow one nudge for this new sequence.
                        loop_nudge_injected = false;
                    }
                    if consecutive_loop >= MAX_CONSECUTIVE_LOOP {
                        let preview: String = blocks
                            .first()
                            .map(|b| {
                                let s = b.code.trim();
                                let chars: String = s.chars().take(120).collect();
                                if s.chars().count() > 120 {
                                    format!("{chars}…")
                                } else {
                                    chars
                                }
                            })
                            .unwrap_or_default();
                        let detail = format!(
                            "{consecutive_loop} consecutive identical iterations (code+output unchanged); repeated code preview: {preview:?}"
                        );
                        if !response.trim().is_empty() {
                            best_partial = Some(response);
                        }
                        return self.limit_outcome(
                            LimitKind::Loop,
                            detail,
                            best_partial,
                            iterations,
                        );
                    }
                }

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
                // Each block carries the provider-assigned id from the sink, so
                // we use it directly — no code-equality heuristic that would
                // collide when two tool calls have identical code.
                // For any tool call that did not produce an executed block (e.g.
                // a non-`repl` tool call whose `code` argument was absent), push
                // a synthetic error result so the provider always sees exactly one
                // tool_result per tool_use in the preceding assistant message.
                let executed_ids: std::collections::HashSet<&str> =
                    blocks.iter().map(|b| b.id.as_str()).collect();
                for block in &blocks {
                    let output = format_execution_result(&block.result);
                    history.push(ChatMessage::tool_result(block.id.clone(), output));
                }
                for tc in &completion.tool_calls {
                    if !executed_ids.contains(tc.id.as_str()) {
                        history.push(ChatMessage::tool_result(
                            tc.id.clone(),
                            format!(
                                "Error: tool call '{}' did not produce executable code",
                                tc.name
                            ),
                        ));
                    }
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

                // Save Point: collect pending steer messages from the caller.
                // They are deferred to the top of the next turn's user message
                // rather than pushed immediately after tool_result messages; a
                // separate Role::User object between the tool_results and the
                // next assistant turn can cause provider validation errors.
                if let Some(sp) = save_point {
                    let queued = sp(i);
                    if !queued.is_empty() {
                        let combined = queued
                            .iter()
                            .map(|m| m.content.clone())
                            .collect::<Vec<_>>()
                            .join("\n\n");
                        pending_steer = Some(combined);
                    }
                }

                // After LOOP_NUDGE_THRESHOLD identical iterations, inject a
                // corrective nudge so the model can course-correct before the
                // hard stop at MAX_CONSECUTIVE_LOOP fires. The flag ensures at
                // most one nudge per contiguous stall sequence even if the model
                // alternates between two stall patterns (which each reset
                // `consecutive_loop` and re-expose the threshold).
                if consecutive_loop == LOOP_NUDGE_THRESHOLD && !loop_nudge_injected {
                    loop_nudge_injected = true;
                    history.push(ChatMessage {
                        role: Role::User,
                        content: LOOP_NUDGE.to_owned(),
                        ..Default::default()
                    });
                }

                i += 1;
                continue;
            }

            // No tool calls: the model emitted pure text (prose).
            // Check for explicit <task_complete> token (fallback for models
            // that don't support native tool calls). Disabled under
            // `ProsePolicy::Strict`: the answer dict is the only completion
            // channel in tool-only mode.
            if self.config.prose_policy == ProsePolicy::Lenient
                && let Some(caps) = TASK_COMPLETE_RE.captures(&response)
            {
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
            // An empty-blocks (prose) turn resets loop detection: the model is
            // not in a code-execution stall if it emitted prose at all.
            prev_loop_sig = None;
            consecutive_loop = 0;
            loop_nudge_injected = false;
            consecutive_prose += 1;
            // Strict (tool-only) mode: prose never becomes the final answer.
            // A hard ceiling still guards against an infinite prose spin —
            // the run fails instead of silently accepting prose.
            if self.config.prose_policy == ProsePolicy::Strict {
                if consecutive_prose >= MAX_CONSECUTIVE_PROSE_STRICT {
                    return Outcome::failed(
                        format!(
                            "Error: {MAX_CONSECUTIVE_PROSE_STRICT} consecutive turns without a tool call under strict prose policy; aborting run"
                        ),
                        iterations,
                    );
                }
            } else if consecutive_prose >= MAX_CONSECUTIVE_PROSE {
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

            // Bank the prose into history and continue.
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

            // After PROSE_NUDGE_THRESHOLD consecutive block-free turns,
            // inject a nudge reminding the model to use the REPL or submit
            // a final answer. Lenient: once, giving the model a chance to
            // course-correct before the safety valve fires. Strict: on every
            // prose turn from the threshold on — there is no valve, so keep
            // pushing until the model calls a tool or the hard ceiling fires.
            let nudge_now = match self.config.prose_policy {
                ProsePolicy::Lenient => consecutive_prose == PROSE_NUDGE_THRESHOLD,
                ProsePolicy::Strict => consecutive_prose >= PROSE_NUDGE_THRESHOLD,
            };
            if nudge_now {
                let nudge = match self.config.prose_policy {
                    ProsePolicy::Lenient => PROSE_NUDGE,
                    ProsePolicy::Strict => STRICT_PROSE_NUDGE,
                };
                history.push(ChatMessage {
                    role: Role::User,
                    content: nudge.to_owned(),
                    ..Default::default()
                });
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
}
