use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum DriverFrame {
    Ready {
        #[serde(default)]
        error: Option<String>,
    },
    Fatal {
        error: String,
    },
    ExecResult {
        id: u64,
        stdout: String,
        stderr: String,
        #[serde(default)]
        vars: Vec<String>,
        #[serde(default)]
        final_answer: Option<String>,
        #[serde(default)]
        truncated: bool,
    },
    Subcall {
        id: u64,
        kind: String,
        #[serde(default)]
        prompt: Option<String>,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        args: Option<Value>,
        #[serde(default)]
        kwargs: Option<Value>,
    },
    AnswerState {
        id: u64,
        content: String,
        ready: bool,
    },
    /// Fire-and-forget notification that a prelude tool started/finished.
    ToolEvent {
        phase: String,
        name: String,
        #[serde(default)]
        call_id: u64,
        #[serde(default)]
        args: Option<Value>,
        #[serde(default)]
        result: Option<String>,
        #[serde(default)]
        error: Option<String>,
        #[serde(default)]
        duration: Option<f64>,
    },
}
