//! Helper functions for the RLM engine: previews, event dispatch,
//! REPL turn reconstruction, and restored-session types.

use std::sync::Arc;

use base64::Engine as _;
use phi_ai::Usage;
use phi_repl::ReplResult;
use serde_json::Value;

use phi_ext::AppRuntime;
use phi_ext::LifecycleEvent;

use crate::llm_parse::{ExecutedBlock, format_iteration};
use crate::lm_core::{ChatMessage, Role, ToolCall, UsageBreakdown};

pub fn truncate_preview(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

pub fn prompt_preview(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_preview(&flat, max)
}

pub fn usage_breakdowns_for_model(model: String, usage: Usage) -> Vec<UsageBreakdown> {
    if usage.total_tokens == 0
        && usage.input == 0
        && usage.output == 0
        && usage.cache_read == 0
        && usage.cache_write == 0
        && usage.cost.total == 0.0
    {
        Vec::new()
    } else {
        vec![UsageBreakdown { model, usage }]
    }
}

/// A persistent multi-turn RLM conversation. The REPL env survives across
/// turns (variables persist); each completed exchange is exposed in the env
/// as `history_N`. Usage accumulates across the whole session.
pub fn history_snippet(index: usize, history: &[ChatMessage], response: &str) -> String {
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

/// A subcall restored from the canonical session stream, carrying the
/// full result text so it can be injected into the REPL on restore without
/// re-calling the subagent.
#[derive(Clone, Debug, Default)]
pub struct RestoredSubcall {
    pub model: String,
    pub prompt_preview: String,
    pub result: Option<String>,
    pub error: Option<String>,
    pub depth: usize,
}

/// A REPL execution restored from the canonical session stream.
#[derive(Clone, Debug)]
pub struct RestoredRepl {
    pub code: String,
    pub stdout: String,
    pub stderr: String,
    pub subcalls: Vec<RestoredSubcall>,
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

/// Append a restored execution as the same (assistant tool call, tool result)
/// message pair the model sees live.
pub fn push_repl_turns(
    history: &mut Vec<ChatMessage>,
    repl: &RestoredRepl,
    max_output_chars: usize,
) {
    history.push(ChatMessage::assistant_with_tools(
        String::new(),
        vec![ToolCall {
            id: String::new(),
            name: "repl".to_owned(),
            arguments: serde_json::json!({ "code": repl.code }),
        }],
    ));
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
        history.push(ChatMessage::tool_result(String::new(), outputs));
    }
}

/// Build a Python snippet that injects a `subcalls_N` variable into the
/// REPL, mirroring `history_snippet`. The variable is a list of dicts with
/// `model`, `prompt_preview`, `result`, `error`, and `depth` — one entry per
/// subcall across all repl blocks in the exchange. The model can index it to
/// recover a subagent's full output without re-calling it, but it is NOT
/// auto-injected into the LM transcript (gatekeeper: model must print to see).
pub fn subcalls_snippet(index: usize, repls: &[RestoredRepl]) -> Option<String> {
    let subcalls: Vec<Value> = repls
        .iter()
        .flat_map(|repl| {
            repl.subcalls.iter().map(|sc| {
                serde_json::json!({
                    "model": sc.model,
                    "prompt_preview": sc.prompt_preview,
                    "result": sc.result,
                    "error": sc.error,
                    "depth": sc.depth,
                })
            })
        })
        .collect();
    if subcalls.is_empty() {
        return None;
    }
    let encoded =
        base64::engine::general_purpose::STANDARD.encode(Value::Array(subcalls).to_string());
    Some(format!(
        "import base64 as _b64, json as _json\nsubcalls_{index} = _json.loads(_b64.b64decode(\"{encoded}\").decode(\"utf-8\"))\ndel _b64, _json"
    ))
}

pub fn context_snippet(index: usize, text: &str) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    format!(
        "import base64 as _b64\ncontext_{index} = _b64.b64decode(\"{encoded}\").decode(\"utf-8\")\ndel _b64"
    )
}

pub fn dispatch_event(runtime: &Option<Arc<AppRuntime>>, event: LifecycleEvent) {
    if let Some(rt) = runtime {
        let rt = rt.clone();
        tokio::spawn(async move {
            let _ = rt.dispatch(event).await;
        });
    }
}
