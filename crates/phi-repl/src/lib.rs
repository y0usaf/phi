//! REPL environments for the RLM engine: `ReplEnv` trait + Python
//! subprocess backend (ndjson over stdio). Later: Lua, Docker.

mod protocol;
pub mod python;

pub use python::PythonRepl;

use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum ReplError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("environment crashed: {0}")]
    Crashed(String),
    #[error("execution timed out after {0:?}")]
    Timeout(Duration),
    #[error("environment not started")]
    NotStarted,
}

/// Environment setup: context payload, prelude configuration, host fns.
#[derive(Clone, Debug)]
pub struct EnvSetup {
    /// Structured context, exposed as `context_0` / `context`.
    pub context: Option<Value>,
    /// String context; takes precedence over `context` if both are set.
    pub context_str: Option<String>,
    /// Code executed once after setup (output discarded).
    pub setup_code: Option<String>,
    /// Working directory for executed code; defaults to a scratch temp dir.
    pub cwd: Option<String>,
    /// Host-registered REPL functions, exposed as blocking shims.
    pub host_fns: Vec<String>,
    pub max_concurrent_subcalls: usize,
    /// Per-stream (stdout/stderr) output cap in characters.
    pub max_output_chars: usize,
}

impl Default for EnvSetup {
    fn default() -> Self {
        Self {
            context: None,
            context_str: None,
            setup_code: None,
            cwd: None,
            host_fns: Vec::new(),
            max_concurrent_subcalls: 4,
            max_output_chars: 20_000,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ReplResult {
    pub stdout: String,
    pub stderr: String,
    /// Names of simple-typed variables currently in the namespace.
    pub vars: Vec<String>,
    /// Captured when the model flipped `answer["ready"] = True` this exec.
    pub final_answer: Option<String>,
    pub duration: Duration,
    pub truncated: bool,
}

#[derive(Clone, Debug, Default)]
pub struct AnswerState {
    pub content: String,
    pub ready: bool,
}

/// A request initiated by code running inside the environment.
#[derive(Clone, Debug)]
pub enum SubcallRequest {
    /// `llm_query`: plain LM completion, no recursion.
    Llm {
        prompt: String,
        model: Option<String>,
    },
    /// `subagent`: recursive RLM sub-call (engine may fall back to plain LM).
    Rlm {
        prompt: String,
        model: Option<String>,
    },
    /// Host-registered REPL function call.
    HostFn {
        name: String,
        args: Value,
        kwargs: Value,
    },
    /// Extension-registered tool call (routes to AppRuntime::invoke_tool)
    ExtTool {
        name: String,
        args: serde_json::Value,
    },
}

#[derive(Clone, Debug)]
pub enum SubcallResponse {
    /// For Llm/Rlm: the response text.
    Text(String),
    /// For HostFn: a JSON value returned to the REPL.
    Value(Value),
}

/// Notification that a tool function ran inside the environment (emitted by
/// the prelude around `bash`, `edit_file`, ... calls). Purely informational:
/// the tool already executed in-env; no response flows back.
#[derive(Clone, Debug)]
pub enum ToolEvent {
    Start {
        call_id: u64,
        name: String,
        args: Value,
    },
    End {
        call_id: u64,
        name: String,
        result: String,
        error: Option<String>,
        duration: Duration,
    },
}

/// Serves subcalls while an `exec` is in flight. Errors surface in the REPL
/// as `"Error: ..."` strings (queries) or raised `RuntimeError` (host fns).
#[async_trait]
pub trait SubcallHandler: Send + Sync {
    async fn handle(&self, request: SubcallRequest) -> Result<SubcallResponse, String>;

    /// Observe in-env tool activity. Default: ignored.
    fn notify_tool(&self, _event: ToolEvent) {}
}

#[async_trait]
pub trait ReplEnv: Send {
    async fn start(&mut self, setup: EnvSetup) -> Result<(), ReplError>;
    /// Execute code in the persistent namespace. On `Timeout`/`Crashed` the
    /// environment is respawned with a fresh namespace (setup re-applied).
    async fn exec(&mut self, code: &str, timeout: Duration) -> Result<ReplResult, ReplError>;
    async fn answer_state(&mut self) -> Result<AnswerState, ReplError>;
    async fn shutdown(&mut self);
}
