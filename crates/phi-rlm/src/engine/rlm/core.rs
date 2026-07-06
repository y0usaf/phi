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
    EnvFactory, Events, LimitKind, PythonEnvFactory, RlmCompletion, RlmConfig, RlmEvent,
    SavePointFn, StopCause,
};
use crate::llm_parse::{ExecutedBlock, format_execution_result};
use crate::lm_core::{ChatMessage, ContextPayload, LanguageModel, LmHandler, Role};
use crate::prompts::{
    DEFAULT_ANSWER_NUDGE, EMPTY_TURN_NUDGE, QueryMetadata, SystemPromptOptions,
    build_system_prompt, metadata_user_prompt,
};

use super::super::TurnState;
use super::super::helpers::{bound_summary, dispatch_await, dispatch_event};
use super::super::state::RunState;
use super::super::subcalls::{EngineSubcalls, Outcome};

use regex_lite::Regex;

static TASK_COMPLETE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)<task_complete\s+output="(?P<output>.*?)"\s*/>"#)
        .expect("task_complete regex")
});

/// Maximum consecutive *empty* turns (no tool calls AND no text) before the
/// run fails. Non-empty prose is the final answer immediately (WS9.1), so
/// only a model emitting nothing at all reaches this — each empty turn gets
/// an `EMPTY_TURN_NUDGE` first, and an empty string can never be accepted as
/// an answer, so the run fails rather than spinning.
const MAX_CONSECUTIVE_EMPTY: usize = 3;

/// When a turn-capped run has this many turns (or fewer) remaining, a
/// remaining-turns reminder is included in the next turn's user preamble
/// (codex's token-budget-reminder pattern). Uncapped root runs never see it.
const TURN_WARNING_THRESHOLD: usize = 3;

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

/// A tool call arriving from the streaming sink, carrying the provider-
/// assigned tool call id, the tool name, and the raw arguments for eager
/// execution. The id is threaded into `ExecutedBlock` so the exec loop can
/// pair each result with the correct `tool_result` message without relying
/// on argument equality.
pub struct ToolCallResult {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// How one streamed tool call executes (WS9.3a): the engine-owned `repl`
/// tool runs its `code` in the env; registry tools dispatch either to their
/// host handler (watchdogged) or to a same-named Python function in the env.
enum ToolRoute {
    Repl(String),
    Handler,
    Python,
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

/// A `ReplResult` carrying only an error, for failed/blocked tool routes.
fn error_result(stderr: String, duration: std::time::Duration) -> ReplResult {
    ReplResult {
        stdout: String::new(),
        stderr,
        vars: Vec::new(),
        final_answer: None,
        final_summary: None,
        duration,
        truncated: false,
    }
}

/// Synthesize the env-side call for a Python-dispatched native tool:
/// `__phi_native_call__("<name>", "<args json>")`. Both literals are JSON-
/// encoded strings, which are valid Python string literals (same escape
/// vocabulary), so arbitrary argument content round-trips safely.
fn native_python_call(name: &str, args: &serde_json::Value) -> String {
    let name_lit = serde_json::to_string(name).unwrap_or_else(|_| "\"\"".to_owned());
    let args_json = serde_json::to_string(args).unwrap_or_else(|_| "{}".to_owned());
    let args_lit = serde_json::to_string(&args_json).unwrap_or_else(|_| "\"{}\"".to_owned());
    format!("__phi_native_call__({name_lit}, {args_lit})")
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

    /// Register the per-turn tool list on the root model: the engine-owned
    /// `repl` tool (unless the active agent's `env.repl_tool` is `Some(false)`,
    /// WS1.2 — the pi-shaped profile hides the REPL surface), plus every
    /// native-exposed tool from the extension registry (WS9.3a). Called
    /// before each run so the list tracks registry state. Bare core (no
    /// agent active, or the agent leaves `env` empty) keeps the `repl` tool —
    /// today's behavior, so bare core boots.
    pub fn register_tools(&self) {
        // The `repl` tool is engine-owned. The active agent's `env.repl_tool`
        // knob controls whether it appears in the model's surface: `None` or
        // `Some(true)` = inherit (on, today's behavior); `Some(false)` = hide
        // it (the pi-shaped flat tool-call loop has no REPL surface).
        let repl_enabled = self
            .runtime
            .as_ref()
            .and_then(|rt| rt.active_agent_repl_tool())
            .unwrap_or(true);
        let mut tools = Vec::new();
        if repl_enabled {
            tools.push(repl_tool_definition());
        }
        if let Some(rt) = &self.runtime {
            for spec in rt.native_tool_specs() {
                // `repl` is engine-registered; a registry tool can't shadow it.
                if spec.name == "repl" {
                    continue;
                }
                tools.push(ToolDefinition {
                    name: spec.name.clone(),
                    description: spec.description.clone(),
                    input_schema: spec.native_input_schema(),
                });
            }
        }
        self.root.set_tools(tools);
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
                self.register_tools();
                let outcome = self
                    .run_loop(&mut env, &handler, &mut history, 0, cancel, None)
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
        // The active agent's `env.context` knob controls whether the
        // `context` / `context_0` variables are injected into the REPL env
        // (WS1.2). `None` or `Some(true)` = inherit (inject, today's
        // behavior); `Some(false)` = skip injection (the pi-shaped profile
        // has no context-variable scaffold). The payload is still threaded
        // to `QueryMetadata` for the user prompt either way — this gate is
        // only about the REPL-side `context` variable, not the prompt.
        let context_enabled = self
            .runtime
            .as_ref()
            .and_then(|rt| rt.active_agent_context())
            .unwrap_or(true);
        let (context, context_str) = if context_enabled {
            (context, context_str)
        } else {
            (None, None)
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
        history_count: usize,
        cancel: &CancellationToken,
        save_point: Option<&SavePointFn>,
    ) -> Outcome {
        let start = Instant::now();
        let mut best_partial: Option<String> = None;
        // Latest bounded `answer['summary']` seen this run (WS7.3). A standing
        // declaration, not tied to `answer['ready']`: it rides out on whichever
        // model-finish path ends the run (answer, prose, default answer).
        let mut run_summary: Option<String> = None;
        let mut consecutive_errors = 0usize;
        let mut consecutive_empty = 0usize;
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
        // In-loop compaction state (successful count + failure breaker).
        let mut compaction = super::compaction::CompactionState::default();

        // Whether the engine-owned `repl` tool is exposed this run (WS1.2).
        // Mirrors `register_tools`: `None`/`Some(true)` = on, `Some(false)` =
        // hidden. A hallucinated `repl` call when the tool is hidden falls
        // through to the unknown-tool path (synthetic error), not the REPL.
        let repl_enabled = self
            .runtime
            .as_ref()
            .and_then(|rt| rt.active_agent_repl_tool())
            .unwrap_or(true);
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

            // ── Per-turn user preamble (WS9.2) ──────────────────────────────
            // No routine "Turn N:" scaffolding is injected — the model just
            // continues from the tool results, like pi/codex. A user message
            // is pushed only when there is real content for this turn:
            // extension context injections (pi's `context` event), steer text
            // carried from the previous turn's save_point, or a remaining-
            // turns reminder when a capped run nears its limit. Providers
            // merge consecutive user-role messages, so pushing this after
            // tool results is safe.
            // Extension-dispatch time for this iteration (WS9.6 telemetry):
            // context injections here, plus the tool interception hooks
            // accumulated inside the exec loop below.
            let mut dispatch_time = std::time::Duration::ZERO;
            let mut preamble: Vec<String> = Vec::new();
            if let Some(rt) = &self.runtime {
                // Race context injection against cancellation. On cancel,
                // yield no injections and let the outer `select!` catch cancel
                // on the next poll — a blocked Context handler can't strand the
                // iteration.
                let dispatch_start = Instant::now();
                let injections = tokio::select! {
                    r = rt.context_injections(i as u32) => r.unwrap_or_default(),
                    () = cancel.cancelled() => Vec::new(),
                };
                dispatch_time += dispatch_start.elapsed();
                for (label, content) in &injections {
                    preamble.push(format!("# {label}\n{content}"));
                }
            }
            if let Some(steer) = pending_steer.take() {
                preamble.push(steer);
            }
            if let Some(limit) = iteration_limit {
                let remaining = limit - i;
                if remaining <= TURN_WARNING_THRESHOLD {
                    preamble.push(format!(
                        "Note: turn {}/{limit} — {remaining} turn(s) remaining. If you cannot finish in time, submit your best answer now.",
                        i + 1
                    ));
                }
            }
            if !preamble.is_empty() {
                history.push(ChatMessage {
                    role: Role::User,
                    content: preamble.join("\n\n"),
                    ..Default::default()
                });
            }

            // Channel for eagerly executing tool calls during the stream.
            // The sender is cloned into the tool_sink closure; we drop our
            // copy so the channel closes when the closure is dropped (after
            // the stream completes), unblocking the exec receiver.
            let (tool_tx, mut tool_rx) = unbounded_channel::<ToolCallResult>();
            let tool_sink = self.tool_call_sink(i, tool_tx.clone());
            drop(tool_tx);

            let stream = async {
                let model_start = Instant::now();
                let result = handler
                    .complete_root(
                        history,
                        Some(self.delta_sink(i)),
                        Some(self.thinking_sink(i)),
                        Some(tool_sink),
                        cancel,
                    )
                    .await;
                (result, model_start.elapsed())
            };

            // Eagerly execute tool calls as they arrive during the stream.
            let exec = async {
                let mut blocks = Vec::new();
                let mut dispatch = std::time::Duration::ZERO;
                while let Some(call) = tool_rx.recv().await {
                    let ToolCallResult {
                        id: tool_call_id,
                        name,
                        arguments,
                    } = call;

                    // Route the call (WS9.3a): engine-owned `repl` executes
                    // its code in the env; any other name resolves against
                    // the registry's native-exposed tools. Unroutable calls
                    // are skipped here — the banking pass below pushes a
                    // synthetic error tool_result for them so the provider
                    // always sees one result per tool_use.
                    let route = if name == "repl" && repl_enabled {
                        match arguments.get("code").and_then(|v| v.as_str()) {
                            Some(code) => ToolRoute::Repl(code.to_owned()),
                            None => continue,
                        }
                    } else {
                        match self
                            .runtime
                            .as_ref()
                            .and_then(|rt| rt.native_tool_dispatch(&name))
                        {
                            Some(phi_ext::ToolDispatch::Handler(_)) => ToolRoute::Handler,
                            Some(phi_ext::ToolDispatch::Python) => ToolRoute::Python,
                            None => continue,
                        }
                    };

                    // ── ToolCall interception ──────────────────────────
                    // Dispatch tool_call event before execution. If any
                    // handler returns ToolBlock, skip execution and inject
                    // a blocked result instead.
                    let dispatch_start = Instant::now();
                    let returns = dispatch_await(
                        &self.runtime,
                        LifecycleEvent {
                            kind: EventKind::ToolCall,
                            payload: EventPayload::ToolCall {
                                name: name.clone(),
                                args: arguments.clone(),
                            },
                        },
                    )
                    .await;
                    dispatch += dispatch_start.elapsed();

                    let blocked = returns
                        .iter()
                        .any(|r| matches!(r, EventReturn::ToolBlock { .. }));

                    // UI events: repl executions keep their dedicated
                    // CodeExec channel; registry tools go out as ToolStart/
                    // ToolEnd (the same vocabulary prelude tool events use),
                    // with a call id hashed from the provider-assigned id.
                    let ui_call_id = {
                        let mut h = DefaultHasher::new();
                        tool_call_id.hash(&mut h);
                        h.finish()
                    };
                    match &route {
                        ToolRoute::Repl(code) => {
                            self.events.emit(RlmEvent::CodeExecStart {
                                depth: self.depth,
                                iteration: i,
                                code: code.clone(),
                            });
                        }
                        _ => {
                            self.events.emit(RlmEvent::ToolStart {
                                depth: self.depth,
                                call_id: ui_call_id,
                                name: name.clone(),
                                args: arguments.clone(),
                            });
                        }
                    }
                    let exec_start = Instant::now();

                    // ── Extension: ToolExecutionStart ──────────────────────
                    if let Some(rt) = &self.runtime {
                        let call_id = format!("d{}i{}", self.depth, i);
                        let dispatch_start = Instant::now();
                        let _ = rt
                            .on_tool_execution_start(&call_id, &name, arguments.clone())
                            .await;
                        dispatch += dispatch_start.elapsed();
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
                        error_result(format!("Tool blocked: {reason}"), exec_start.elapsed())
                    } else {
                        match &route {
                            ToolRoute::Repl(code) => env
                                .exec(code, self.config.exec_timeout)
                                .await
                                .unwrap_or_else(|e| {
                                    error_result(e.to_string(), exec_start.elapsed())
                                }),
                            ToolRoute::Python => {
                                // Same implementation, second exposure: the
                                // structured args call the same-named prelude
                                // function in the same env, so hashline cache,
                                // read-before-edit, and spill are shared.
                                let code = native_python_call(&name, &arguments);
                                env.exec(&code, self.config.exec_timeout)
                                    .await
                                    .unwrap_or_else(|e| {
                                        error_result(e.to_string(), exec_start.elapsed())
                                    })
                            }
                            ToolRoute::Handler => {
                                // Host handler with the doctrine-2 watchdog
                                // (timeout enforced inside `invoke_tool`).
                                let rt = self.runtime.as_ref().expect("routed via runtime");
                                match rt.invoke_tool(&name, arguments.clone()).await {
                                    Ok(Some(output)) => ReplResult {
                                        stdout: output.content,
                                        stderr: String::new(),
                                        vars: Vec::new(),
                                        final_answer: None,
                                        final_summary: None,
                                        duration: exec_start.elapsed(),
                                        truncated: false,
                                    },
                                    Ok(None) => error_result(
                                        format!("Error: no tool named '{name}'"),
                                        exec_start.elapsed(),
                                    ),
                                    Err(e) => {
                                        error_result(format!("Error: {e}"), exec_start.elapsed())
                                    }
                                }
                            }
                        }
                    };

                    // ── ToolResult interception ───────────────────────
                    // Dispatch tool_result event after execution. If any
                    // handler returns ToolResultOverride, use the override
                    // content instead of the actual result.
                    // ── Extension: ToolExecutionEnd ────────────────────────
                    if let Some(rt) = &self.runtime {
                        let call_id = format!("d{}i{}", self.depth, i);
                        let dispatch_start = Instant::now();
                        let _ = rt
                            .on_tool_execution_end(
                                &call_id,
                                &name,
                                serde_json::json!({
                                    "stdout": repl_result.stdout,
                                    "stderr": repl_result.stderr,
                                    "duration": repl_result.duration.as_secs_f64(),
                                }),
                                !repl_result.stderr.is_empty(),
                            )
                            .await;
                        dispatch += dispatch_start.elapsed();
                    }

                    let result_str = format_execution_result(&repl_result);
                    let dispatch_start = Instant::now();
                    let returns = dispatch_await(
                        &self.runtime,
                        LifecycleEvent {
                            kind: EventKind::ToolResult,
                            payload: EventPayload::ToolResult {
                                name: name.clone(),
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
                    dispatch += dispatch_start.elapsed();

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

                    match &route {
                        ToolRoute::Repl(_) => {
                            self.events.emit(RlmEvent::CodeExecEnd {
                                depth: self.depth,
                                iteration: i,
                                stdout: final_result.stdout.clone(),
                                stderr: final_result.stderr.clone(),
                                duration: final_result.duration,
                            });
                        }
                        _ => {
                            self.events.emit(RlmEvent::ToolEnd {
                                depth: self.depth,
                                call_id: ui_call_id,
                                name: name.clone(),
                                result: final_result.stdout.clone(),
                                error: (!final_result.stderr.is_empty())
                                    .then(|| final_result.stderr.clone()),
                                duration: final_result.duration,
                            });
                        }
                    }
                    blocks.push(ExecutedBlock {
                        id: tool_call_id,
                        name,
                        args: arguments,
                        result: final_result,
                    });
                }
                (blocks, dispatch)
            };

            let ((completion, model_time), (blocks, exec_dispatch)) = tokio::select! {
                pair = async { tokio::join!(stream, exec) } => pair,
                () = cancel.cancelled() => {
                    return self.cancelled_outcome(best_partial, iterations);
                }
            };
            dispatch_time += exec_dispatch;
            let exec_time: std::time::Duration = blocks.iter().map(|b| b.result.duration).sum();
            if let Some(s) = blocks
                .iter()
                .rev()
                .find_map(|b| b.result.final_summary.as_deref())
            {
                run_summary = bound_summary(s);
            }
            let completion = match completion {
                Ok(completion) => completion,
                Err(e) => {
                    let message = format!("Error: root completion failed - {e}");
                    return match best_partial {
                        Some(partial) => Outcome {
                            stop: StopCause::Failed,
                            response: partial,
                            summary: None,
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
                    model: model_time,
                    exec: exec_time,
                    dispatch: dispatch_time,
                });
                return Outcome {
                    stop: StopCause::Answer,
                    response: answer,
                    summary: run_summary,
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
                consecutive_empty = 0;

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
                            b.name.hash(&mut h);
                            b.args.to_string().hash(&mut h);
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
                                let source = b.source();
                                let s = source.trim();
                                let chars: String = s.chars().take(120).collect();
                                if s.chars().count() > 120 {
                                    format!("{chars}…")
                                } else {
                                    chars
                                }
                            })
                            .unwrap_or_default();
                        let detail = format!(
                            "{consecutive_loop} consecutive identical iterations (calls+output unchanged); repeated call preview: {preview:?}"
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
                // For any tool call that did not produce an executed block
                // (a `repl` call without `code`, or a name that isn't a
                // native-exposed registry tool), push a synthetic error
                // result so the provider always sees exactly one tool_result
                // per tool_use in the preceding assistant message.
                let executed_ids: std::collections::HashSet<&str> =
                    blocks.iter().map(|b| b.id.as_str()).collect();
                // Aggregate truncation budget: each result is already capped
                // per-stream by the driver, but a turn with many repl calls
                // must not inject N full-size results into history at once.
                let mut aggregate_budget = self
                    .config
                    .max_output_chars
                    .saturating_mul(crate::llm_parse::AGGREGATE_OUTPUT_FACTOR);
                for block in &blocks {
                    let output = crate::llm_parse::clamp_aggregate(
                        format_execution_result(&block.result),
                        &mut aggregate_budget,
                    );
                    history.push(ChatMessage::tool_result(block.id.clone(), output));
                }
                for tc in &completion.tool_calls {
                    if !executed_ids.contains(tc.id.as_str()) {
                        history.push(ChatMessage::tool_result(
                            tc.id.clone(),
                            format!(
                                "Error: tool call '{}' was not executed (unknown tool or missing required arguments)",
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
                    model: model_time,
                    exec: exec_time,
                    dispatch: dispatch_time,
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

                // In-loop compaction: fold history into a summary when the
                // last completion's usage shows the context window filling.
                self.maybe_compact(
                    env,
                    handler,
                    history,
                    &completion.usage,
                    &mut compaction,
                    i,
                    history_count,
                    cancel,
                )
                .await;

                i += 1;
                continue;
            }

            // No tool calls: the model emitted pure text (prose).
            // Check for explicit `<task_complete>` token (fallback for models
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
                    model: model_time,
                    exec: exec_time,
                    dispatch: dispatch_time,
                });
                return Outcome {
                    stop: StopCause::Prose,
                    response: final_answer,
                    summary: run_summary,
                    error: None,
                    iterations,
                };
            }

            // A block-free turn resets loop detection: the model is not in a
            // code-execution stall if it stopped calling tools.
            prev_loop_sig = None;
            consecutive_loop = 0;
            loop_nudge_injected = false;

            // WS9.1: a no-tool-call, non-empty turn IS the final answer —
            // immediately, matching our own system prompt ("stop making tool
            // calls and output your answer as text") and pi/codex turn
            // semantics. `answer['ready']` and `<task_complete>` above remain
            // the explicit alternate completion paths.
            let prose = response.trim();
            if !prose.is_empty() {
                let prose = prose.to_owned();
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
                    model: model_time,
                    exec: exec_time,
                    dispatch: dispatch_time,
                });
                return Outcome {
                    stop: StopCause::Prose,
                    response: prose,
                    summary: run_summary,
                    error: None,
                    iterations,
                };
            }

            // Empty turn: no tool calls AND no text. Nothing to bank (an
            // empty assistant message is provider-hostile) and nothing that
            // could be an answer. Nudge every empty turn — without new input
            // the model faces an identical context and would likely repeat —
            // and fail at the ceiling rather than spin.
            consecutive_empty += 1;
            if consecutive_empty >= MAX_CONSECUTIVE_EMPTY {
                let message = format!(
                    "Error: {MAX_CONSECUTIVE_EMPTY} consecutive empty model turns (no tool calls, no text); aborting run"
                );
                return match best_partial {
                    Some(partial) => Outcome {
                        stop: StopCause::Failed,
                        response: partial,
                        summary: None,
                        error: Some(message),
                        iterations,
                    },
                    None => Outcome::failed(message, iterations),
                };
            }
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
                model: model_time,
                exec: exec_time,
                dispatch: dispatch_time,
            });
            history.push(ChatMessage {
                role: Role::User,
                content: EMPTY_TURN_NUDGE.to_owned(),
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
                    summary: run_summary,
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
                        summary: None,
                        error: Some(message),
                        iterations,
                    },
                    None => Outcome::failed(message, iterations),
                }
            }
        }
    }
}
