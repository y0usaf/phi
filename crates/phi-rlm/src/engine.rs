//! The RLM root loop, ported from `rlm/core/rlm.py` `RLM.completion()`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine as _;
use phi_ai::Usage;
use phi_repl::{
    EnvSetup, PythonRepl, ReplEnv, ReplResult, SubcallHandler, SubcallRequest, SubcallResponse,
    ToolEvent,
};
use serde_json::Value;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio_util::sync::CancellationToken;

use crate::handler::{DeltaSink, LanguageModel, LmHandler, ThinkingSink};
use crate::parsing::{
    ExecutedBlock, could_open_block, find_code_blocks, format_iteration, last_block_end,
};
use crate::prompts::{
    DEFAULT_ANSWER_NUDGE, PROSE_NUDGE, QueryMetadata, build_system_prompt, build_user_prompt,
    metadata_user_prompt,
};
use crate::{ChatMessage, ContextPayload, Role};

#[derive(Clone, Debug)]
pub struct RlmConfig {
    pub max_depth: usize,
    pub max_iterations: usize,
    pub max_budget: Option<f64>,
    pub max_timeout: Option<Duration>,
    pub max_tokens: Option<u64>,
    pub max_errors: Option<usize>,
    pub max_concurrent_subcalls: usize,
    pub max_output_chars: usize,
    pub exec_timeout: Duration,
    pub custom_system_prompt: Option<String>,
    pub user_prologue: Option<String>,
    pub orchestrator: bool,
    /// Pre-formatted custom-tools listing for the system prompt.
    pub tools_section: Option<String>,
    /// Code executed once at env startup (e.g. the coding-agent prelude).
    pub setup_code: Option<String>,
    /// Working directory for REPL code; defaults to a scratch temp dir.
    pub cwd: Option<String>,
    /// Host-registered REPL function names, injected as driver shims.
    pub host_fns: Vec<String>,
}

impl Default for RlmConfig {
    fn default() -> Self {
        Self {
            max_depth: 1,
            max_iterations: 30,
            max_budget: None,
            max_timeout: None,
            max_tokens: None,
            max_errors: None,
            max_concurrent_subcalls: 4,
            max_output_chars: 20_000,
            exec_timeout: Duration::from_secs(120),
            custom_system_prompt: None,
            user_prologue: None,
            orchestrator: true,
            tools_section: None,
            setup_code: None,
            cwd: None,
            host_fns: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LimitKind {
    Budget,
    Timeout,
    Tokens,
    Errors,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StopCause {
    /// The model flipped `answer["ready"] = True`.
    Answer,
    /// Two consecutive turns with no ```repl``` block: after one corrective
    /// reminder, the second prose-only message is accepted as the answer.
    Prose,
    /// Iterations exhausted; answer synthesized from the trajectory.
    DefaultAnswer,
    /// `depth >= max_depth`: plain LM completion, no REPL.
    Fallback,
    Limit(LimitKind),
    Cancelled,
    /// Root model or environment failure.
    Failed,
}

#[derive(Clone, Debug)]
pub struct RlmCompletion {
    pub response: String,
    pub usage: Usage,
    pub iterations: usize,
    pub duration: Duration,
    pub stop: StopCause,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub enum RlmEvent {
    RunStart {
        depth: usize,
    },
    IterationStart {
        depth: usize,
        iteration: usize,
    },
    IterationEnd {
        depth: usize,
        iteration: usize,
        duration: Duration,
    },
    /// Streamed root-model text chunk (only when the model streams).
    RootDelta {
        depth: usize,
        iteration: usize,
        delta: String,
    },
    /// Streamed provider-thinking chunk for the current root completion.
    RootThinkingDelta {
        depth: usize,
        iteration: usize,
        delta: String,
    },
    /// Full root-model response for the iteration.
    RootText {
        depth: usize,
        iteration: usize,
        text: String,
    },
    CodeExecStart {
        depth: usize,
        iteration: usize,
        code: String,
    },
    CodeExecEnd {
        depth: usize,
        iteration: usize,
        stdout: String,
        stderr: String,
        duration: Duration,
    },
    /// A prelude tool (bash, edit_file, ...) started inside this run's REPL.
    ToolStart {
        depth: usize,
        call_id: u64,
        name: String,
        args: Value,
    },
    ToolEnd {
        depth: usize,
        call_id: u64,
        name: String,
        result: String,
        error: Option<String>,
        duration: Duration,
    },
    SubcallStart {
        depth: usize,
        model: String,
        prompt_preview: String,
    },
    SubcallEnd {
        depth: usize,
        model: String,
        #[allow(dead_code)]
        result_preview: Option<String>,
        duration: Duration,
        error: Option<String>,
    },
    AnswerSubmitted {
        depth: usize,
        content: String,
    },
    LimitHit {
        depth: usize,
        limit: LimitKind,
        detail: String,
    },
    RunEnd {
        depth: usize,
        stop: StopCause,
        iterations: usize,
        duration: Duration,
    },
}

#[derive(Clone, Default)]
pub struct Events(Option<UnboundedSender<RlmEvent>>);

impl Events {
    pub fn new(sender: UnboundedSender<RlmEvent>) -> Self {
        Self(Some(sender))
    }

    fn emit(&self, event: RlmEvent) {
        if let Some(sender) = &self.0 {
            let _ = sender.send(event);
        }
    }
}

/// Creates a fresh `ReplEnv` per completion, wired to the engine's
/// subcall handler.
pub trait EnvFactory: Send + Sync {
    fn create(&self, handler: Arc<dyn SubcallHandler>) -> Box<dyn ReplEnv>;
}

pub struct PythonEnvFactory {
    pub python: String,
}

impl Default for PythonEnvFactory {
    fn default() -> Self {
        Self {
            python: "python3".into(),
        }
    }
}

impl EnvFactory for PythonEnvFactory {
    fn create(&self, handler: Arc<dyn SubcallHandler>) -> Box<dyn ReplEnv> {
        Box::new(PythonRepl::with_python(self.python.clone(), handler))
    }
}

/// Shared per-run state for subcall handlers. The handler outlives a single
/// run in sessions (the env persists across `send()` calls), so the current
/// cancel token and run start time live behind a mutex refreshed by `begin`.
#[derive(Clone)]
struct RunState(Arc<Mutex<(CancellationToken, Instant)>>);

impl RunState {
    fn new() -> Self {
        Self(Arc::new(Mutex::new((
            CancellationToken::new(),
            Instant::now(),
        ))))
    }

    fn begin(&self, cancel: CancellationToken) {
        *self.0.lock().unwrap() = (cancel, Instant::now());
    }

    fn cancel(&self) -> CancellationToken {
        self.0.lock().unwrap().0.clone()
    }

    fn started(&self) -> Instant {
        self.0.lock().unwrap().1
    }
}

fn truncate_preview(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

fn prompt_preview(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_preview(&flat, max)
}

/// Routes subcall frames from the REPL: `llm_query` to the handler's sub
/// model, `rlm_query` to a child `Rlm` at `depth + 1` (falling back to a
/// plain completion at max depth, matching upstream), host fns to the
/// registered host handler.
struct EngineSubcalls {
    rlm: Rlm,
    handler: Arc<LmHandler>,
    run: RunState,
}

impl EngineSubcalls {
    async fn query(
        &self,
        prompt: String,
        model: Option<String>,
        kind: &str,
    ) -> Result<SubcallResponse, String> {
        let model_name = self.handler.sub_name(model.as_deref());
        self.rlm.events.emit(RlmEvent::SubcallStart {
            depth: self.rlm.depth + 1,
            model: model_name.clone(),
            prompt_preview: prompt_preview(&prompt, 80),
        });
        let start = Instant::now();
        let result = self.handler.complete_sub(&prompt, model.as_deref()).await;
        let (outcome, error, result_preview) = match result {
            Ok(completion) => {
                let preview = truncate_preview(&completion.text, 240);
                (
                    Ok(SubcallResponse::Text(completion.text)),
                    None,
                    Some(preview),
                )
            }
            Err(e) => (Err(format!("{kind} query failed - {e}")), Some(e), None),
        };
        self.rlm.events.emit(RlmEvent::SubcallEnd {
            depth: self.rlm.depth + 1,
            model: model_name,
            result_preview,
            duration: start.elapsed(),
            error,
        });
        outcome
    }

    /// Upstream `RLM._subcall`: spawn a child RLM with its own REPL at
    /// `depth + 1`, or fall back to a plain completion at max depth. The
    /// child inherits the parent's config with the *remaining* budget and
    /// timeout; its usage rolls up into the parent's handler. Limit/failure
    /// outcomes come back as error text, not an exception in the REPL.
    async fn recurse(
        &self,
        prompt: String,
        model: Option<String>,
    ) -> Result<SubcallResponse, String> {
        let next_depth = self.rlm.depth + 1;
        let parent = &self.rlm.config;
        if next_depth >= parent.max_depth {
            return self.query(prompt, model, "RLM").await;
        }

        let mut config = parent.clone();
        config.user_prologue = None;
        let usage = self.handler.usage();
        if let Some(max_budget) = parent.max_budget {
            let remaining = max_budget - usage.cost.total;
            if remaining <= 0.0 {
                return Ok(SubcallResponse::Text(format!(
                    "Error: Budget exhausted (spent ${:.6} of ${:.6})",
                    usage.cost.total, max_budget
                )));
            }
            config.max_budget = Some(remaining);
        }
        if let Some(max_timeout) = parent.max_timeout {
            let elapsed = self.run.started().elapsed();
            if elapsed >= max_timeout {
                return Ok(SubcallResponse::Text(format!(
                    "Error: Timeout exhausted ({:.1}s of {:.1}s)",
                    elapsed.as_secs_f64(),
                    max_timeout.as_secs_f64()
                )));
            }
            config.max_timeout = Some(max_timeout - elapsed);
        }

        // A model override becomes the child's root; unknown names fall back
        // to the default sub model, consistent with `llm_query` routing.
        let root = match model.as_deref() {
            Some(name) => self
                .rlm
                .named
                .get(name)
                .cloned()
                .unwrap_or_else(|| self.rlm.sub.clone()),
            None => self.rlm.root.clone(),
        };
        let model_name = root.name();
        self.rlm.events.emit(RlmEvent::SubcallStart {
            depth: next_depth,
            model: model_name.clone(),
            prompt_preview: prompt_preview(&prompt, 80),
        });
        let start = Instant::now();

        let child = Rlm {
            config,
            root,
            sub: self.rlm.sub.clone(),
            named: self.rlm.named.clone(),
            envs: self.rlm.envs.clone(),
            host_fns: self.rlm.host_fns.clone(),
            events: self.rlm.events.clone(),
            depth: next_depth,
        };
        let cancel = self.run.cancel();
        let completion = child
            .completion(ContextPayload::Str(prompt), None, &cancel)
            .await;
        self.handler.track(&completion.usage);
        let result_preview = if completion.response.trim().is_empty() {
            None
        } else {
            Some(truncate_preview(&completion.response, 240))
        };
        self.rlm.events.emit(RlmEvent::SubcallEnd {
            depth: next_depth,
            model: model_name,
            result_preview,
            duration: start.elapsed(),
            error: completion.error.clone(),
        });
        Ok(SubcallResponse::Text(completion.response))
    }
}

#[async_trait]
impl SubcallHandler for EngineSubcalls {
    fn notify_tool(&self, event: ToolEvent) {
        let depth = self.rlm.depth;
        self.rlm.events.emit(match event {
            ToolEvent::Start {
                call_id,
                name,
                args,
            } => RlmEvent::ToolStart {
                depth,
                call_id,
                name,
                args,
            },
            ToolEvent::End {
                call_id,
                name,
                result,
                error,
                duration,
            } => RlmEvent::ToolEnd {
                depth,
                call_id,
                name,
                result,
                error,
                duration,
            },
        });
    }

    async fn handle(&self, request: SubcallRequest) -> Result<SubcallResponse, String> {
        match request {
            SubcallRequest::Llm { prompt, model } => self.query(prompt, model, "LM").await,
            SubcallRequest::Rlm { prompt, model } => self.recurse(prompt, model).await,
            host_fn @ SubcallRequest::HostFn { .. } => match &self.rlm.host_fns {
                Some(handler) => handler.handle(host_fn).await,
                None => {
                    let SubcallRequest::HostFn { name, .. } = host_fn else {
                        unreachable!()
                    };
                    Err(format!("unknown function: {name}"))
                }
            },
        }
    }
}

/// Result of one root-loop run, before env lifecycle and `RunEnd` handling.
struct Outcome {
    stop: StopCause,
    response: String,
    error: Option<String>,
    iterations: usize,
}

impl Outcome {
    fn failed(message: String, iterations: usize) -> Self {
        Self {
            stop: StopCause::Failed,
            response: message.clone(),
            error: Some(message),
            iterations,
        }
    }
}

#[derive(Clone)]
pub struct Rlm {
    config: RlmConfig,
    root: Arc<dyn LanguageModel>,
    sub: Arc<dyn LanguageModel>,
    named: HashMap<String, Arc<dyn LanguageModel>>,
    envs: Arc<dyn EnvFactory>,
    host_fns: Option<Arc<dyn SubcallHandler>>,
    events: Events,
    depth: usize,
}

impl Rlm {
    pub fn new(
        config: RlmConfig,
        root: Arc<dyn LanguageModel>,
        sub: Arc<dyn LanguageModel>,
        envs: Arc<dyn EnvFactory>,
    ) -> Self {
        Self {
            config,
            root,
            sub,
            named: HashMap::new(),
            envs,
            host_fns: None,
            events: Events::default(),
            depth: 0,
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

    pub fn with_events(mut self, sender: UnboundedSender<RlmEvent>) -> Self {
        self.events = Events::new(sender);
        self
    }

    pub fn at_depth(mut self, depth: usize) -> Self {
        self.depth = depth;
        self
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
            pending_shell: Vec::new(),
        }
    }

    pub async fn completion(
        &self,
        payload: ContextPayload,
        root_prompt: Option<&str>,
        cancel: &CancellationToken,
    ) -> RlmCompletion {
        let start = Instant::now();
        self.events.emit(RlmEvent::RunStart { depth: self.depth });

        if self.depth >= self.config.max_depth {
            return self.fallback(payload, start).await;
        }

        let handler = Arc::new(LmHandler::new(
            self.root.clone(),
            self.sub.clone(),
            self.named.clone(),
        ));
        let run = RunState::new();
        run.begin(cancel.clone());

        let metadata = QueryMetadata::from_payload(&payload);
        let outcome = match self.start_env(&handler, payload, run).await {
            Ok(mut env) => {
                let mut history = self.initial_history(metadata, root_prompt, 0);
                let outcome = self
                    .run_loop(&mut env, &handler, &mut history, 1, 0, cancel)
                    .await;
                env.shutdown().await;
                outcome
            }
            Err(message) => Outcome::failed(message, 0),
        };
        self.complete_run(outcome, &handler, start)
    }

    async fn start_env(
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
        let mut env = self.envs.create(subcalls);
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

    fn initial_history(
        &self,
        metadata: QueryMetadata,
        root_prompt: Option<&str>,
        _history_count: usize,
    ) -> Vec<ChatMessage> {
        let mut history = vec![
            ChatMessage {
                role: Role::System,
                content: build_system_prompt(
                    self.config.custom_system_prompt.as_deref(),
                    self.config.tools_section.as_deref(),
                    self.config.orchestrator,
                ),
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

    async fn run_loop(
        &self,
        env: &mut Box<dyn ReplEnv>,
        handler: &Arc<LmHandler>,
        history: &mut Vec<ChatMessage>,
        context_count: usize,
        history_count: usize,
        cancel: &CancellationToken,
    ) -> Outcome {
        let start = Instant::now();
        let mut best_partial: Option<String> = None;
        let mut consecutive_errors = 0usize;
        let mut iterations = 0usize;
        let mut prose_nudged = false;

        for i in 0..self.config.max_iterations {
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
                content: build_user_prompt(
                    i,
                    self.config.max_iterations,
                    context_count,
                    history_count,
                ),
            });

            self.events.emit(RlmEvent::IterationStart {
                depth: self.depth,
                iteration: i,
            });
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
                    let mut sent = sent.lock().unwrap();
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

            // Enforce REPL-first behavior on the opening turn of a cold run:
            // a plain prose reply with no executed blocks means the model
            // ignored the environment. Instead of banking that as progress,
            // bounce it with an explicit reminder and continue the loop.
            // Warm sends (history_count > 0) skip this: follow-ups may
            // legitimately be answered in prose from prior context.
            if i == 0 && history_count == 0 && blocks.is_empty() {
                history.push(ChatMessage {
                    role: Role::Assistant,
                    content: response,
                });
                history.push(ChatMessage {
                    role: Role::User,
                    content: "You must inspect the `context` via the REPL before answering. Write a ```repl``` block that probes the environment (for example, print a small slice, length, or keys) and continue from there.".to_owned(),
                });
                self.events.emit(RlmEvent::IterationEnd {
                    depth: self.depth,
                    iteration: i,
                    duration: iter_start.elapsed(),
                });
                continue;
            }

            // A turn with no ```repl``` block executed nothing and submitted
            // nothing: re-prompting with a bare "Turn N:" gives the model no
            // new information, so it re-emits the same prose until the
            // iteration budget burns out. Instead, nudge once with a
            // corrective reminder; a second consecutive block-free turn is
            // accepted as the final answer.
            if blocks.is_empty() && !response.trim().is_empty() {
                best_partial = Some(response.clone());
                if prose_nudged {
                    self.events.emit(RlmEvent::IterationEnd {
                        depth: self.depth,
                        iteration: i,
                        duration: iter_start.elapsed(),
                    });
                    return Outcome {
                        stop: StopCause::Prose,
                        response,
                        error: None,
                        iterations,
                    };
                }
                prose_nudged = true;
                history.push(ChatMessage {
                    role: Role::Assistant,
                    content: response,
                });
                history.push(ChatMessage {
                    role: Role::User,
                    content: PROSE_NUDGE.to_owned(),
                });
                self.events.emit(RlmEvent::IterationEnd {
                    depth: self.depth,
                    iteration: i,
                    duration: iter_start.elapsed(),
                });
                continue;
            }
            if !blocks.is_empty() {
                prose_nudged = false;
            }

            if !response.trim().is_empty() {
                best_partial = Some(response.clone());
            }
            if let Some(answer) = final_answer {
                self.events.emit(RlmEvent::AnswerSubmitted {
                    depth: self.depth,
                    content: answer.clone(),
                });
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
            self.events.emit(RlmEvent::IterationEnd {
                depth: self.depth,
                iteration: i,
                duration: iter_start.elapsed(),
            });
        }

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
            let mut buffer = buffer.lock().unwrap();
            buffer.push_str(delta);
            let blocks = find_code_blocks(&buffer);
            let mut sent = sent.lock().unwrap();
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
    async fn fallback(&self, payload: ContextPayload, start: Instant) -> RlmCompletion {
        let content = match payload {
            ContextPayload::Str(s) => s,
            ContextPayload::Json(Value::String(s)) => s,
            ContextPayload::Json(v) => v.to_string(),
        };
        let messages = [ChatMessage {
            role: Role::User,
            content,
        }];
        let (response, stop, error) = match self.root.complete(&messages).await {
            Ok(completion) => (completion.text, StopCause::Fallback, None),
            Err(e) => (
                format!("Error: LM query failed at max depth - {e}"),
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
            usage: Usage::default(),
            iterations: 0,
            duration,
            stop,
            error,
        }
    }

    fn complete_run(&self, outcome: Outcome, handler: &LmHandler, start: Instant) -> RlmCompletion {
        let duration = start.elapsed();
        self.events.emit(RlmEvent::RunEnd {
            depth: self.depth,
            stop: outcome.stop,
            iterations: outcome.iterations,
            duration,
        });
        RlmCompletion {
            response: outcome.response,
            usage: handler.usage(),
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

/// A persistent multi-turn RLM conversation. The REPL env survives across
/// turns (variables persist); each completed exchange is exposed in the env
/// as `history_N`. Usage accumulates across the whole session.
pub struct RlmSession {
    rlm: Rlm,
    payload: ContextPayload,
    handler: Arc<LmHandler>,
    run: RunState,
    env: Option<Box<dyn ReplEnv>>,
    exchanges: usize,
    contexts: usize,
    /// In-band conversational transcript: prior (user prompt, final answer)
    /// pairs replayed verbatim on every warm `send`, so follow-ups are real
    /// continuations instead of cold starts. Full per-run trajectories still
    /// land in the env as `history_N` variables (the archive tier).
    transcript: Vec<ChatMessage>,
    /// User-initiated shell executions to replay to the model on the next
    /// `send`, as (assistant ```repl``` block, REPL-output user message)
    /// pairs — the model sees them as its own prior repl turns.
    pending_shell: Vec<ChatMessage>,
}

impl RlmSession {
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
        self.rlm.events.emit(RlmEvent::RunStart { depth: 0 });
        self.run.begin(cancel.clone());

        let fresh_env = self.env.is_none();
        if let Err(message) = self.ensure_env().await {
            return self
                .rlm
                .complete_run(Outcome::failed(message, 0), &self.handler, start);
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

        let env = self.env.as_mut().expect("env started above");
        let metadata = QueryMetadata::from_payload(&self.payload);
        let mut history = if self.transcript.is_empty() {
            self.rlm
                .initial_history(metadata, Some(prompt), self.exchanges)
        } else {
            // Warm send: replay the conversation so far in-band, then append
            // the new prompt as its own user message.
            let mut history = self.rlm.initial_history(metadata, None, self.exchanges);
            history.extend(self.transcript.iter().cloned());
            history.append(&mut self.pending_shell);
            history.push(ChatMessage {
                role: Role::User,
                content: format!("Answer the following: {prompt}"),
            });
            history
        };
        // Cold send: replay user shell executions after the prompt (the warm
        // branch drained them above, before the prompt).
        history.append(&mut self.pending_shell);
        let outcome = self
            .rlm
            .run_loop(
                env,
                &self.handler,
                &mut history,
                self.contexts,
                self.exchanges,
                cancel,
            )
            .await;

        if !outcome.response.trim().is_empty() && outcome.stop != StopCause::Failed {
            let snippet = history_snippet(self.exchanges, &history, &outcome.response);
            let _ = env.exec(&snippet, self.rlm.config.exec_timeout).await;
            self.exchanges += 1;
            self.transcript.push(ChatMessage {
                role: Role::User,
                content: format!("Answer the following: {prompt}"),
            });
            self.transcript.push(ChatMessage {
                role: Role::Assistant,
                content: outcome.response.clone(),
            });
        }
        self.rlm.complete_run(outcome, &self.handler, start)
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
            });
            for repl in &exchange.repls {
                push_repl_turns(&mut history, repl, self.rlm.config.max_output_chars);
            }
            let snippet = history_snippet(self.exchanges, &history, &exchange.response);
            env.exec(&snippet, self.rlm.config.exec_timeout)
                .await
                .map_err(|e| format!("failed to restore history_{}: {e}", self.exchanges))?;
            self.exchanges += 1;
            self.transcript.push(ChatMessage {
                role: Role::User,
                content: format!("Answer the following: {}", exchange.prompt),
            });
            self.transcript.push(ChatMessage {
                role: Role::Assistant,
                content: exchange.response.clone(),
            });
        }
        for repl in pending {
            push_repl_turns(
                &mut self.pending_shell,
                repl,
                self.rlm.config.max_output_chars,
            );
        }
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
        self.pending_shell.clear();
    }

    pub async fn shutdown(&mut self) {
        if let Some(mut env) = self.env.take() {
            env.shutdown().await;
        }
    }
}

/// Full message trajectory of the run plus the final response, as upstream
/// persistent-mode `add_history`. The system prompt is omitted: it is
/// identical every exchange, so storing it per-history would only duplicate
/// it in the REPL.
fn history_snippet(index: usize, history: &[ChatMessage], response: &str) -> String {
    let mut messages: Vec<Value> = history
        .iter()
        .filter(|m| m.role != Role::System)
        .map(|m| serde_json::json!({ "role": m.role, "content": m.content }))
        .collect();
    messages.push(serde_json::json!({ "role": "assistant", "content": response }));
    let encoded =
        base64::engine::general_purpose::STANDARD.encode(Value::Array(messages).to_string());
    let alias = if index == 0 {
        "\nhistory = history_0"
    } else {
        ""
    };
    format!(
        "import base64 as _b64, json as _json\nhistory_{index} = _json.loads(_b64.b64decode(\"{encoded}\").decode(\"utf-8\")){alias}\ndel _b64, _json"
    )
}

/// A REPL execution restored from the canonical session stream.
#[derive(Clone, Debug)]
pub struct RestoredRepl {
    pub code: String,
    pub stdout: String,
    pub stderr: String,
}

/// One chat exchange restored from the canonical session stream, with the
/// REPL executions that ran inside it (`repls`) and any user shell escapes
/// recorded just before its prompt (`pre_shell`).
#[derive(Clone, Debug, Default)]
pub struct RestoredExchange {
    pub pre_shell: Vec<RestoredRepl>,
    pub prompt: String,
    pub repls: Vec<RestoredRepl>,
    pub response: String,
}

/// Append a restored execution as the same (assistant code, user output)
/// message pair the model sees live.
fn push_repl_turns(history: &mut Vec<ChatMessage>, repl: &RestoredRepl, max_output_chars: usize) {
    history.push(ChatMessage {
        role: Role::Assistant,
        content: format!("```repl\n{}\n```", repl.code),
    });
    let block = ExecutedBlock {
        code: repl.code.clone(),
        result: ReplResult {
            stdout: repl.stdout.clone(),
            stderr: repl.stderr.clone(),
            vars: Vec::new(),
            final_answer: None,
            duration: std::time::Duration::ZERO,
            truncated: false,
        },
    };
    if let Some(outputs) = format_iteration(std::slice::from_ref(&block), max_output_chars) {
        history.push(ChatMessage {
            role: Role::User,
            content: outputs,
        });
    }
}

fn context_snippet(index: usize, text: &str) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    format!(
        "import base64 as _b64\ncontext_{index} = _b64.b64decode(\"{encoded}\").decode(\"utf-8\")\ndel _b64"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handler::Completion;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    struct ScriptedLm {
        name: String,
        responses: Mutex<VecDeque<String>>,
        prompts: Mutex<Vec<Vec<ChatMessage>>>,
        default: String,
        usage_per_call: Usage,
    }

    impl ScriptedLm {
        fn new(name: &str, responses: &[&str]) -> Self {
            Self {
                name: name.into(),
                responses: Mutex::new(responses.iter().map(|s| (*s).to_owned()).collect()),
                prompts: Mutex::new(Vec::new()),
                default: String::new(),
                usage_per_call: Usage::default(),
            }
        }

        fn with_default(mut self, default: &str) -> Self {
            self.default = default.into();
            self
        }

        fn with_usage(mut self, usage: Usage) -> Self {
            self.usage_per_call = usage;
            self
        }

        fn recorded_prompts(&self) -> Vec<Vec<ChatMessage>> {
            self.prompts.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl LanguageModel for ScriptedLm {
        fn name(&self) -> String {
            self.name.clone()
        }

        async fn complete(&self, prompt: &[ChatMessage]) -> Result<Completion, String> {
            self.prompts.lock().unwrap().push(prompt.to_vec());
            let text = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| self.default.clone());
            Ok(Completion {
                text,
                usage: self.usage_per_call,
            })
        }
    }

    fn rlm(config: RlmConfig, root: Arc<ScriptedLm>, sub: Arc<ScriptedLm>) -> Rlm {
        Rlm::new(config, root, sub, Arc::new(PythonEnvFactory::default()))
    }

    fn quiet_sub() -> Arc<ScriptedLm> {
        Arc::new(ScriptedLm::new("sub-model", &[]).with_default("sub default"))
    }

    #[tokio::test]
    async fn answers_via_answer_protocol() {
        let root = Arc::new(ScriptedLm::new(
            "root-model",
            &[
                "Probing.\n```repl\nprint(context[:20])\n```",
                "Submitting.\n```repl\nanswer['content'] = context.split()[-1]\nanswer['ready'] = True\n```",
            ],
        ));
        let engine = rlm(RlmConfig::default(), root.clone(), quiet_sub());
        let result = engine
            .completion(
                ContextPayload::Str("The magic number is 42".into()),
                Some("What is the magic number?"),
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result.stop, StopCause::Answer);
        assert_eq!(result.response, "42");
        assert_eq!(result.iterations, 2);

        let prompts = root.recorded_prompts();
        assert_eq!(prompts[0][0].role, Role::System);
        assert!(
            prompts[0][0]
                .content
                .starts_with("You are a Recursive Language Model")
        );
        assert!(
            prompts[0][1]
                .content
                .starts_with("Answer the following: What is the magic number?")
        );
        // Second turn sees the REPL output of the first.
        let second = &prompts[1];
        assert!(second.iter().any(|m| m.content.contains("REPL output:")));
    }

    #[tokio::test]
    async fn routes_llm_query_subcalls() {
        let root = Arc::new(ScriptedLm::new(
            "root-model",
            &[
                "```repl\nr = llm_query('what?')\nprint(r)\n```",
                "```repl\nanswer['content'] = r\nanswer['ready'] = True\n```",
            ],
        ));
        let sub = Arc::new(ScriptedLm::new("sub-model", &[]).with_default("sub says hi"));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let engine = rlm(RlmConfig::default(), root.clone(), sub).with_events(tx);
        let result = engine
            .completion(
                ContextPayload::Str("ctx".into()),
                None,
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result.stop, StopCause::Answer);
        assert_eq!(result.response, "sub says hi");

        let mut saw_subcall = false;
        let mut saw_root_text = false;
        while let Ok(event) = rx.try_recv() {
            match event {
                RlmEvent::SubcallStart { model, .. } => {
                    assert_eq!(model, "sub-model");
                    saw_subcall = true;
                }
                RlmEvent::RootText { .. } => saw_root_text = true,
                _ => {}
            }
        }
        assert!(saw_subcall);
        assert!(saw_root_text);
    }

    /// Streams a fence-closing chunk, then refuses to finish the stream until
    /// the block starts executing — proving exec overlaps the stream.
    struct EagerLm {
        exec_started: Arc<tokio::sync::Notify>,
    }

    #[async_trait]
    impl LanguageModel for EagerLm {
        fn name(&self) -> String {
            "eager-model".into()
        }

        async fn complete(&self, _prompt: &[ChatMessage]) -> Result<Completion, String> {
            Err("streaming expected".into())
        }

        async fn complete_streaming(
            &self,
            _prompt: &[ChatMessage],
            on_delta: DeltaSink,
            _on_thinking: Option<ThinkingSink>,
        ) -> Result<Completion, String> {
            let head =
                "Running.\n```repl\nanswer['content'] = 'done'\nanswer['ready'] = True\n```\n";
            assert!(on_delta(head), "no trailing prose yet: keep streaming");
            tokio::time::timeout(Duration::from_secs(10), self.exec_started.notified())
                .await
                .map_err(|_| "block was not executed during the stream".to_owned())?;
            assert!(!on_delta("tail"), "prose after a block must abort");
            Ok(Completion {
                text: format!("{head}tail"),
                usage: Usage::default(),
            })
        }
    }

    #[tokio::test]
    async fn executes_blocks_while_streaming() {
        let exec_started = Arc::new(tokio::sync::Notify::new());
        let root = Arc::new(EagerLm {
            exec_started: exec_started.clone(),
        });
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                if matches!(event, RlmEvent::CodeExecStart { .. }) {
                    exec_started.notify_one();
                }
            }
        });
        let engine = Rlm::new(
            RlmConfig::default(),
            root,
            quiet_sub(),
            Arc::new(PythonEnvFactory::default()),
        )
        .with_events(tx);
        let result = engine
            .completion(
                ContextPayload::Str("ctx".into()),
                None,
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result.stop, StopCause::Answer, "{:?}", result.error);
        assert_eq!(result.response, "done");
    }

    #[tokio::test]
    async fn multi_block_outputs_batch_together() {
        let root = Arc::new(ScriptedLm::new(
            "root-model",
            &[
                "Two at once.\n```repl\nprint('alpha')\n```\n```repl\nprint('beta')\n```",
                "```repl\nanswer['content'] = 'ok'\nanswer['ready'] = True\n```",
            ],
        ));
        let engine = rlm(RlmConfig::default(), root.clone(), quiet_sub());
        let result = engine
            .completion(
                ContextPayload::Str("ctx".into()),
                None,
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result.stop, StopCause::Answer, "{:?}", result.error);

        // Both block outputs land in one user message on the next turn.
        let prompts = root.recorded_prompts();
        let feedback = prompts[1]
            .iter()
            .find(|m| m.role == Role::User && m.content.contains("REPL output"))
            .expect("feedback message");
        assert!(feedback.content.contains("REPL output (block 1):"));
        assert!(feedback.content.contains("alpha"));
        assert!(feedback.content.contains("REPL output (block 2):"));
        assert!(feedback.content.contains("beta"));
    }

    #[tokio::test]
    async fn trailing_prose_discarded_from_history() {
        let root = Arc::new(ScriptedLm::new(
            "root-model",
            &[
                "Plan.\n```repl\nprint('x')\n```\nThis prose must vanish.",
                "```repl\nanswer['content'] = 'ok'\nanswer['ready'] = True\n```",
            ],
        ));
        let engine = rlm(RlmConfig::default(), root.clone(), quiet_sub());
        let result = engine
            .completion(
                ContextPayload::Str("ctx".into()),
                None,
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result.stop, StopCause::Answer, "{:?}", result.error);

        // The assistant turn in history ends at the closing fence.
        let prompts = root.recorded_prompts();
        let assistant = prompts[1]
            .iter()
            .find(|m| m.role == Role::Assistant)
            .expect("assistant message");
        assert!(assistant.content.ends_with("```"), "{}", assistant.content);
        assert!(!assistant.content.contains("vanish"));
    }

    #[tokio::test]
    async fn first_turn_plain_prose_is_bounced_until_repl_used() {
        let root = Arc::new(ScriptedLm::new(
            "root-model",
            &[
                "I can answer without using the REPL.",
                "Probing now.\n```repl\nprint(context[:4])\n```",
                "```repl\nanswer['content'] = context.split()[-1]\nanswer['ready'] = True\n```",
            ],
        ));
        let engine = rlm(RlmConfig::default(), root.clone(), quiet_sub());
        let result = engine
            .completion(
                ContextPayload::Str("The magic number is 42".into()),
                Some("What is the magic number?"),
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result.stop, StopCause::Answer, "{:?}", result.error);
        assert_eq!(result.response, "42");
        assert_eq!(result.iterations, 3);

        let prompts = root.recorded_prompts();
        let second = &prompts[1];
        assert!(
            second
                .iter()
                .any(|m| m.role == Role::Assistant && m.content.contains("without using the REPL"))
        );
        let reminder = second
            .iter()
            .find(|m| {
                m.role == Role::User
                    && m.content
                        .contains("You must inspect the `context` via the REPL")
            })
            .expect("bounce reminder");
        assert!(reminder.content.contains("```repl```"));
    }

    #[tokio::test]
    async fn default_answer_on_iteration_exhaustion() {
        let root = Arc::new(ScriptedLm::new(
            "root-model",
            &["thinking 1", "thinking 2", "the synthesized final"],
        ));
        let config = RlmConfig {
            max_iterations: 2,
            ..RlmConfig::default()
        };
        let engine = rlm(config, root.clone(), quiet_sub());
        let result = engine
            .completion(
                ContextPayload::Str("ctx".into()),
                None,
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result.stop, StopCause::DefaultAnswer);
        assert_eq!(result.response, "the synthesized final");

        let prompts = root.recorded_prompts();
        let last = prompts.last().unwrap().last().unwrap();
        assert_eq!(last.role, Role::Assistant);
        assert_eq!(last.content, DEFAULT_ANSWER_NUDGE);
    }

    #[tokio::test]
    async fn consecutive_errors_hit_limit() {
        let root = Arc::new(
            ScriptedLm::new("root-model", &[]).with_default("trying\n```repl\nundefined_fn()\n```"),
        );
        let config = RlmConfig {
            max_errors: Some(2),
            max_iterations: 5,
            ..RlmConfig::default()
        };
        let engine = rlm(config, root, quiet_sub());
        let result = engine
            .completion(
                ContextPayload::Str("ctx".into()),
                None,
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result.stop, StopCause::Limit(LimitKind::Errors));
        assert_eq!(result.iterations, 2);
        assert!(result.response.starts_with("trying"));
        assert!(result.error.unwrap().contains("Error threshold exceeded"));
    }

    #[tokio::test]
    async fn budget_limit_stops_run() {
        let mut usage = Usage::default();
        usage.cost.total = 1.0;
        let root = Arc::new(
            ScriptedLm::new("root-model", &[])
                .with_default("partial progress")
                .with_usage(usage),
        );
        let config = RlmConfig {
            max_budget: Some(0.5),
            ..RlmConfig::default()
        };
        let engine = rlm(config, root, quiet_sub());
        let result = engine
            .completion(
                ContextPayload::Str("ctx".into()),
                None,
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result.stop, StopCause::Limit(LimitKind::Budget));
        assert_eq!(result.response, "partial progress");
        assert!(result.usage.cost.total >= 1.0);
    }

    #[tokio::test]
    async fn timeout_checked_before_iteration() {
        let root = Arc::new(ScriptedLm::new("root-model", &[]).with_default("never used"));
        let config = RlmConfig {
            max_timeout: Some(Duration::ZERO),
            ..RlmConfig::default()
        };
        let engine = rlm(config, root.clone(), quiet_sub());
        let result = engine
            .completion(
                ContextPayload::Str("ctx".into()),
                None,
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result.stop, StopCause::Limit(LimitKind::Timeout));
        assert_eq!(result.iterations, 0);
        assert!(root.recorded_prompts().is_empty());
    }

    #[tokio::test]
    async fn rlm_query_spawns_child_rlm() {
        // Parent and child share the same root model: responses pop in
        // call order — parent iter 1, child iters 1-2, parent iter 2.
        let root = Arc::new(
            ScriptedLm::new(
                "root-model",
                &[
                    "Delegating.\n```repl\nr = rlm_query('child task')\nprint(r)\n```",
                    "Probing.\n```repl\nprint(context)\n```",
                    "```repl\nanswer['content'] = 'child saw: ' + context\nanswer['ready'] = True\n```",
                    "```repl\nanswer['content'] = r\nanswer['ready'] = True\n```",
                ],
            )
            .with_usage(Usage {
                input: 1,
                ..Usage::default()
            }),
        );
        let config = RlmConfig {
            max_depth: 2,
            ..RlmConfig::default()
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let engine = rlm(config, root.clone(), quiet_sub()).with_events(tx);
        let result = engine
            .completion(
                ContextPayload::Str("parent ctx".into()),
                None,
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result.stop, StopCause::Answer, "{:?}", result.error);
        assert_eq!(result.response, "child saw: child task");
        // Child usage (2 calls) rolls up into the parent's (2 calls).
        assert_eq!(result.usage.input, 4);

        // The child gets the rlm_query prompt as its own context.
        let prompts = root.recorded_prompts();
        assert!(
            prompts[1][1]
                .content
                .contains("Your context is a str of 10 total characters"),
            "{}",
            prompts[1][1].content
        );

        let mut saw_child_run = false;
        let mut saw_child_answer = false;
        let mut subcall_model = None;
        while let Ok(event) = rx.try_recv() {
            match event {
                RlmEvent::RunStart { depth: 1 } => saw_child_run = true,
                RlmEvent::RunEnd { depth: 1, stop, .. } => assert_eq!(stop, StopCause::Answer),
                RlmEvent::AnswerSubmitted { depth: 1, .. } => saw_child_answer = true,
                RlmEvent::SubcallStart {
                    depth: 1, model, ..
                } => subcall_model = Some(model),
                _ => {}
            }
        }
        assert!(saw_child_run);
        assert!(saw_child_answer);
        assert_eq!(subcall_model.as_deref(), Some("root-model"));
    }

    #[tokio::test]
    async fn llm_query_preview_is_single_line() {
        let root = Arc::new(ScriptedLm::new(
            "root-model",
            &[
                "```repl\nr = llm_query(\"\"\"first line\n\nsecond\tline\"\"\")\n```",
                "```repl\nanswer['content'] = r\nanswer['ready'] = True\n```",
            ],
        ));
        let sub = Arc::new(ScriptedLm::new("sub-model", &[]).with_default("ok"));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let engine = rlm(RlmConfig::default(), root, sub).with_events(tx);
        let result = engine
            .completion(
                ContextPayload::Str("ctx".into()),
                None,
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result.stop, StopCause::Answer, "{:?}", result.error);

        let mut preview = None;
        while let Ok(event) = rx.try_recv() {
            if let RlmEvent::SubcallStart {
                depth: 1,
                prompt_preview,
                ..
            } = event
            {
                preview = Some(prompt_preview);
            }
        }
        assert_eq!(preview.as_deref(), Some("first line second line"));
    }

    #[tokio::test]
    async fn rlm_query_preview_is_single_line() {
        let root = Arc::new(ScriptedLm::new(
            "root-model",
            &[
                "```repl\nr = rlm_query(\"\"\"first line\n\nsecond\tline\"\"\")\n```",
                "```repl\nanswer['content'] = 'child ok'\nanswer['ready'] = True\n```",
                "```repl\nanswer['content'] = r\nanswer['ready'] = True\n```",
            ],
        ));
        let config = RlmConfig {
            max_depth: 2,
            ..RlmConfig::default()
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let engine = rlm(config, root, quiet_sub()).with_events(tx);
        let result = engine
            .completion(
                ContextPayload::Str("ctx".into()),
                None,
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result.stop, StopCause::Answer, "{:?}", result.error);

        let mut preview = None;
        while let Ok(event) = rx.try_recv() {
            if let RlmEvent::SubcallStart {
                depth: 1,
                prompt_preview,
                ..
            } = event
            {
                preview = Some(prompt_preview);
            }
        }
        assert_eq!(preview.as_deref(), Some("first line second line"));
    }

    #[tokio::test]
    async fn rlm_query_model_override_roots_the_child() {
        let root = Arc::new(ScriptedLm::new(
            "root-model",
            &[
                "```repl\nr = rlm_query('task', model='fast')\n```",
                "```repl\nanswer['content'] = r\nanswer['ready'] = True\n```",
            ],
        ));
        let fast = Arc::new(ScriptedLm::new(
            "fast-model",
            &["```repl\nanswer['content'] = 'from fast'\nanswer['ready'] = True\n```"],
        ));
        let config = RlmConfig {
            max_depth: 2,
            ..RlmConfig::default()
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut engine = rlm(config, root, quiet_sub());
        engine.register_model("fast", fast);
        let engine = engine.with_events(tx);
        let result = engine
            .completion(
                ContextPayload::Str("ctx".into()),
                None,
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result.stop, StopCause::Answer, "{:?}", result.error);
        assert_eq!(result.response, "from fast");

        let mut subcall_model = None;
        while let Ok(event) = rx.try_recv() {
            if let RlmEvent::SubcallStart {
                depth: 1, model, ..
            } = event
            {
                subcall_model = Some(model);
            }
        }
        assert_eq!(subcall_model.as_deref(), Some("fast-model"));
    }

    #[tokio::test]
    async fn rlm_query_at_max_depth_is_plain_completion() {
        // Default max_depth = 1: no child run, the sub model answers.
        let root = Arc::new(ScriptedLm::new(
            "root-model",
            &[
                "```repl\nr = rlm_query('q')\n```",
                "```repl\nanswer['content'] = r\nanswer['ready'] = True\n```",
            ],
        ));
        let sub = Arc::new(ScriptedLm::new("sub-model", &[]).with_default("sub says deep"));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let engine = rlm(RlmConfig::default(), root, sub).with_events(tx);
        let result = engine
            .completion(
                ContextPayload::Str("ctx".into()),
                None,
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result.stop, StopCause::Answer, "{:?}", result.error);
        assert_eq!(result.response, "sub says deep");

        while let Ok(event) = rx.try_recv() {
            assert!(
                !matches!(event, RlmEvent::RunStart { depth: 1 }),
                "no child RLM at max depth"
            );
        }
    }

    #[tokio::test]
    async fn rlm_query_budget_exhausted_blocks_child_spawn() {
        let mut usage = Usage::default();
        usage.cost.total = 1.0;
        let root = Arc::new(
            ScriptedLm::new("root-model", &[])
                .with_default("```repl\nprint(rlm_query('q'))\n```")
                .with_usage(usage),
        );
        let config = RlmConfig {
            max_depth: 2,
            max_budget: Some(0.5),
            ..RlmConfig::default()
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let engine = rlm(config, root, quiet_sub()).with_events(tx);
        let result = engine
            .completion(
                ContextPayload::Str("ctx".into()),
                None,
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result.stop, StopCause::Limit(LimitKind::Budget));

        let mut saw_exhausted = false;
        while let Ok(event) = rx.try_recv() {
            match event {
                RlmEvent::CodeExecEnd { stdout, .. } => {
                    saw_exhausted |= stdout.contains("Error: Budget exhausted");
                }
                RlmEvent::RunStart { depth } => assert_eq!(depth, 0),
                _ => {}
            }
        }
        assert!(saw_exhausted, "REPL should see the budget-exhausted error");
    }

    #[tokio::test]
    async fn falls_back_to_plain_lm_at_max_depth() {
        let root = Arc::new(ScriptedLm::new("root-model", &["plain answer"]));
        let engine = rlm(RlmConfig::default(), root.clone(), quiet_sub()).at_depth(1);
        let result = engine
            .completion(
                ContextPayload::Str("just answer this".into()),
                None,
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result.stop, StopCause::Fallback);
        assert_eq!(result.response, "plain answer");
        let prompts = root.recorded_prompts();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0][0].content, "just answer this");
    }

    #[tokio::test]
    async fn session_persists_env_and_exposes_history() {
        let root = Arc::new(ScriptedLm::new(
            "root-model",
            &[
                // Turn 1: stash a variable, answer.
                "```repl\nstash = 'kept'\nanswer['content'] = 'first answer'\nanswer['ready'] = True\n```",
                // Turn 2: prove the env persisted and history is visible.
                "```repl\nprint(stash)\nprint(history[-1]['content'])\n```",
                "```repl\nanswer['content'] = 'second answer'\nanswer['ready'] = True\n```",
            ],
        ));
        let engine = rlm(RlmConfig::default(), root.clone(), quiet_sub());
        let mut session = engine.into_session(ContextPayload::Str("ctx".into()));
        let cancel = CancellationToken::new();

        let first = session.send("question one", &cancel).await;
        assert_eq!(first.stop, StopCause::Answer);
        assert_eq!(first.response, "first answer");

        let second = session.send("question two", &cancel).await;
        assert_eq!(second.stop, StopCause::Answer);
        assert_eq!(second.response, "second answer");

        let prompts = root.recorded_prompts();
        // Turn 2's first user prompt mentions the prior history variable.
        let turn2_user = &prompts[1]
            .iter()
            .map(|m| m.content.clone())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            turn2_user.contains("1 prior conversation history"),
            "{turn2_user}"
        );
        // Turn 2's second root call sees the REPL output proving persistence.
        let outputs = prompts[2]
            .iter()
            .map(|m| m.content.clone())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(outputs.contains("kept"), "{outputs}");
        assert!(outputs.contains("first answer"), "{outputs}");

        session.shutdown().await;
    }

    #[tokio::test]
    async fn session_exec_shell_records_repl_turn() {
        let root = Arc::new(ScriptedLm::new(
            "root-model",
            &["```repl\nanswer['content'] = 'done'\nanswer['ready'] = True\n```"],
        ));
        let engine = rlm(RlmConfig::default(), root.clone(), quiet_sub());
        let mut session = engine.into_session(ContextPayload::Str("ctx".into()));
        let cancel = CancellationToken::new();

        let recorded = session
            .exec_shell("print('shell out')", true)
            .await
            .unwrap();
        assert!(recorded.stdout.contains("shell out"));
        let silent = session.exec_shell("print('quiet')", false).await.unwrap();
        assert!(silent.stdout.contains("quiet"));

        let result = session.send("next task", &cancel).await;
        assert_eq!(result.stop, StopCause::Answer);

        // The recorded execution appears as a faked repl turn; the
        // unrecorded one does not.
        let prompts = root.recorded_prompts();
        let roles: Vec<String> = prompts[0]
            .iter()
            .map(|m| format!("{:?}: {}", m.role, m.content))
            .collect();
        let joined = roles.join("\n---\n");
        assert!(
            joined.contains("Assistant: ```repl\nprint('shell out')\n```"),
            "{joined}"
        );
        assert!(joined.contains("REPL output:"), "{joined}");
        assert!(joined.contains("shell out"), "{joined}");
        assert!(!joined.contains("quiet"), "{joined}");

        session.shutdown().await;
    }

    #[tokio::test]
    async fn session_add_context_versions_variables() {
        let root = Arc::new(ScriptedLm::new(
            "root-model",
            &["```repl\nanswer['content'] = context_1.upper()\nanswer['ready'] = True\n```"],
        ));
        let engine = rlm(RlmConfig::default(), root.clone(), quiet_sub());
        let mut session = engine.into_session(ContextPayload::Str("ctx".into()));
        let cancel = CancellationToken::new();

        let index = session.add_context("extra payload").await.unwrap();
        assert_eq!(index, 1);

        let result = session.send("use the extra context", &cancel).await;
        assert_eq!(result.stop, StopCause::Answer);
        assert_eq!(result.response, "EXTRA PAYLOAD");

        // The per-turn prompt advertises both contexts.
        let prompts = root.recorded_prompts();
        let joined = prompts[0]
            .iter()
            .map(|m| m.content.clone())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("2 contexts available"), "{joined}");

        session.shutdown().await;
    }
}
