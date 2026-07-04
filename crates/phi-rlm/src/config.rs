//! Engine config types: RlmConfig, events, limits, and the env factory.

use std::sync::Arc;
use std::time::Duration;

use phi_ai::Usage;
use phi_repl::{PythonRepl, ReplEnv, SubcallHandler};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use crate::lm_core::ChatMessage;
use crate::lm_core::UsageBreakdown;

pub(crate) type SavePointFn = dyn Fn(usize) -> Vec<ChatMessage> + Send + Sync;

#[derive(Clone, Debug)]
pub struct RlmConfig {
    pub max_rlm_depth: usize,
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
    /// Pre-formatted `# Environment` block for the system prompt (cwd, is-git,
    /// platform, shell, OS version). Caller-supplied and memoized per session
    /// (computed once at session build, not per turn). Emitted before `cwd`.
    pub env_info: Option<String>,
    /// Code executed once at env startup (e.g. the coding-agent prelude).
    pub setup_code: Option<String>,
    /// Working directory for REPL code; defaults to a scratch temp dir.
    pub cwd: Option<String>,
    /// Content appended to the system prompt (e.g. from ~/.config/phi/APPEND_SYSTEM.md).
    pub append_system_prompt: Option<String>,
    /// Host-registered REPL function names, injected as driver shims.
    pub host_fns: Vec<String>,
}

impl Default for RlmConfig {
    fn default() -> Self {
        Self {
            max_rlm_depth: 1,
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
            env_info: None,
            setup_code: None,
            cwd: None,
            append_system_prompt: None,
            host_fns: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitKind {
    Budget,
    Timeout,
    Tokens,
    Errors,
    /// The engine detected that the model repeatedly executed the same code
    /// and received identical output — no progress was being made.
    Loop,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StopCause {
    /// The model flipped `answer["ready"] = True`.
    Answer,
    /// Iterations exhausted; answer synthesized from the trajectory.
    DefaultAnswer,
    /// `depth >= max_rlm_depth`: plain LM completion, no REPL.
    Fallback,
    Limit(LimitKind),
    Cancelled,
    /// The model signalled completion without a ```repl``` block: either an
    /// explicit `<task_complete output="..."/>` token, or the
    /// `MAX_CONSECUTIVE_PROSE` safety valve fired after several consecutive
    /// block-free turns. The prose/answer is accepted as the final answer.
    Prose,
    /// Root model or environment failure.
    Failed,
    /// Extension handled the input; agent loop did not run.
    Handled,
}

#[derive(Clone, Debug)]
pub struct RlmCompletion {
    pub response: String,
    /// Bounded (≤4K chars) model-authored report set via `answer['summary']`
    /// (WS7.3). `None` when the model never set one; consumers derive their
    /// own bounded head of `response` if they need an always-present value.
    pub summary: Option<String>,
    pub usage: Usage,
    pub usage_by_model: Vec<UsageBreakdown>,
    pub iterations: usize,
    pub duration: Duration,
    pub stop: StopCause,
    pub error: Option<String>,
}

/// Serialize `Duration` as seconds (f64) for NDJSON output.
fn duration_secs<S: serde::Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_f64(d.as_secs_f64())
}

#[derive(Clone, Debug, Serialize)]
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
        /// Wall-clock duration of the whole iteration.
        #[serde(serialize_with = "duration_secs")]
        duration: Duration,
        /// Time inside the root completion (request + stream). Overlaps
        /// `exec` — tool calls execute eagerly while the stream continues —
        /// so the parts are raw attributions, not an additive breakdown.
        #[serde(serialize_with = "duration_secs")]
        model: Duration,
        /// Total REPL execution time (sum over this iteration's blocks).
        #[serde(serialize_with = "duration_secs")]
        exec: Duration,
        /// Time spent in extension event dispatch (context injections,
        /// tool_call/tool_result interception, execution start/end hooks).
        #[serde(serialize_with = "duration_secs")]
        dispatch: Duration,
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
        #[serde(serialize_with = "duration_secs")]
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
        #[serde(serialize_with = "duration_secs")]
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
        result_preview: Option<String>,
        /// Full subagent response text, carried alongside the preview so
        /// the event handler can persist it durably in `SubcallRecord`.
        result: Option<String>,
        #[serde(serialize_with = "duration_secs")]
        duration: Duration,
        error: Option<String>,
        /// Tokens/cost consumed by this subcall. For `subagent` calls this
        /// covers the child's entire subtree, so deeper `SubcallEnd` events
        /// are already included in their ancestors' usage — sum only one
        /// depth level when aggregating. `None` when the call failed before
        /// usage was known.
        #[serde(skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
    },
    AnswerSubmitted {
        depth: usize,
        content: String,
    },
    /// In-loop context compaction fired (or failed). On success `summary`
    /// carries the text that replaced the compacted span — frontends persist
    /// it as an additive session-log marker. On failure `error` is set and
    /// the engine's circuit breaker advances.
    Compaction {
        depth: usize,
        iteration: usize,
        /// Estimated context tokens that triggered the compaction.
        tokens_before: u64,
        summary: Option<String>,
        error: Option<String>,
    },
    /// Chat message sent from REPL code via `message(...)` — display-only,
    /// does not end the turn or the run.
    Message {
        depth: usize,
        text: String,
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
        #[serde(serialize_with = "duration_secs")]
        duration: Duration,
    },
}

#[derive(Clone, Default)]
pub struct Events(Option<UnboundedSender<RlmEvent>>);

impl Events {
    pub fn new(sender: UnboundedSender<RlmEvent>) -> Self {
        Self(Some(sender))
    }

    pub fn emit(&self, event: RlmEvent) {
        if let Some(sender) = &self.0 {
            let _ = sender.send(event);
        }
    }
}

/// Creates a fresh `ReplEnv` per completion, wired to the engine's
/// subcall handler.
pub(crate) trait EnvFactory: Send + Sync {
    fn create(&self, handler: Arc<dyn SubcallHandler>) -> Box<dyn ReplEnv>;
}

pub(crate) struct PythonEnvFactory {
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
