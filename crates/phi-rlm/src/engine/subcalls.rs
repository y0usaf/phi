//! Subcall routing: `llm_query` to the sub model, `subagent` to a child
//! RLM, host fns to the registered handler.

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use phi_repl::{SubcallHandler, SubcallRequest, SubcallResponse, ToolEvent};
use serde_json::json;

use crate::lm_core::ContextPayload;
use crate::lm_core::LmHandler;

use super::helpers::{prompt_preview, truncate_preview};
use super::rlm::Rlm;
use super::state::RunState;
use crate::config::{RlmEvent, StopCause};

/// Serialize a `StopCause` to the flat string form exposed to REPL code.
///
/// Unit variants follow the serde `snake_case` rename: `answer`, `default_answer`,
/// `fallback`, `prose`, `failed`, `cancelled`, `handled`. The tuple variant
/// `Limit(kind)` becomes `limit_<kind>` (e.g. `limit_budget`, `limit_timeout`)
/// so that new `LimitKind` variants added by other tracks automatically produce
/// the right string without an exhaustive match here.
fn stop_cause_str(stop: &StopCause) -> String {
    match stop {
        StopCause::Limit(kind) => {
            // Serialize the inner LimitKind (snake_case derive), strip quotes,
            // and prepend "limit_" so we get e.g. "limit_budget".
            let inner = serde_json::to_string(kind).unwrap_or_else(|_| "\"unknown\"".into());
            // inner is a JSON string like `"budget"` — strip the surrounding quotes.
            let bare = inner.trim_matches('"');
            format!("limit_{bare}")
        }
        other => {
            // Unit variants serialize to a JSON string like `"default_answer"`.
            let s = serde_json::to_string(other).unwrap_or_else(|_| "\"failed\"".into());
            s.trim_matches('"').to_owned()
        }
    }
}

/// `ok` is true only for the "happy" stops where the child produced completed work.
/// `default_answer` (turn-starved), any limit, `failed`, `cancelled`, `handled`
/// all produce `ok = false`, making degradation visible to the caller.
fn stop_is_ok(stop: &StopCause) -> bool {
    matches!(
        stop,
        StopCause::Answer | StopCause::Prose | StopCause::Fallback
    )
}

pub struct EngineSubcalls {
    pub rlm: Rlm,
    pub handler: Arc<LmHandler>,
    pub run: RunState,
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
        let cancel = self.run.cancel();
        let result = self
            .handler
            .complete_sub(&prompt, model.as_deref(), &cancel)
            .await;
        let (outcome, error, result_preview, full_result) = match result {
            Ok(completion) => {
                let preview = truncate_preview(&completion.text, 240);
                let full = Some(completion.text.clone());
                (
                    Ok(SubcallResponse::Text(completion.text)),
                    None,
                    Some(preview),
                    full,
                )
            }
            Err(e) => (
                Err(format!("{kind} query failed - {e}")),
                Some(e),
                None,
                None,
            ),
        };
        self.rlm.events.emit(RlmEvent::SubcallEnd {
            depth: self.rlm.depth + 1,
            model: model_name,
            result_preview,
            result: full_result,
            duration: start.elapsed(),
            error,
        });
        outcome
    }

    /// Upstream `RLM._subcall`: spawn a child RLM with its own REPL at
    /// `depth + 1`, or fall back to a plain completion at max depth. The
    /// child inherits the parent's config with the *remaining* budget and
    /// timeout; its usage rolls up into the parent's handler.
    ///
    /// Returns `SubcallResponse::Json` in both paths so the REPL always sees
    /// the structured dict `{'text', 'stop', 'iterations', 'ok'}`. The
    /// depth-cap path sets `stop='fallback'` so the caller can distinguish
    /// "ran without a REPL" from a child that finished normally.
    async fn recurse(
        &self,
        prompt: String,
        model: Option<String>,
        preamble: Option<String>,
    ) -> Result<SubcallResponse, String> {
        let next_depth = self.rlm.depth + 1;
        let parent = &self.rlm.config;
        if next_depth >= parent.max_rlm_depth {
            // Depth cap: degrade to a flat one-shot completion. Wrap the result
            // in the same dict shape so the caller can detect the fallback.
            // stop='fallback', iterations=0 (no REPL loop), ok=true (error-free
            // flat completion). On query error the Err propagates verbatim so
            // the REPL sees it as an error string rather than a partial dict.
            let flat = self.query(prompt, model, "RLM").await;
            return match flat {
                Ok(SubcallResponse::Text(text)) => Ok(SubcallResponse::Json(json!({
                    "text": text,
                    "stop": "fallback",
                    "iterations": 0,
                    "ok": true,
                }))),
                other => other,
            };
        }

        let mut config = parent.clone();
        // Forward the caller's preamble as the child's task brief so it starts
        // with parent context rather than a blank slate.
        config.user_prologue = preamble;
        let usage = self.handler.usage();
        if let Some(max_budget) = parent.max_budget {
            let remaining = max_budget - usage.cost.total;
            if remaining <= 0.0 {
                // Return the structured dict so callers can branch on r['ok'] without
                // a TypeError — a plain SubcallResponse::Text would arrive as a bare
                // string rather than the expected {'text', 'stop', 'iterations', 'ok'}.
                return Ok(SubcallResponse::Json(json!({
                    "text": format!(
                        "Error: Budget exhausted (spent ${:.6} of ${:.6})",
                        usage.cost.total, max_budget
                    ),
                    "stop": "failed",
                    "iterations": 0,
                    "ok": false,
                })));
            }
            config.max_budget = Some(remaining);
        }
        if let Some(max_timeout) = parent.max_timeout {
            let elapsed = self.run.started().elapsed();
            if elapsed >= max_timeout {
                // Same structured dict contract as the budget path above.
                return Ok(SubcallResponse::Json(json!({
                    "text": format!(
                        "Error: Timeout exhausted ({:.1}s of {:.1}s)",
                        elapsed.as_secs_f64(),
                        max_timeout.as_secs_f64()
                    ),
                    "stop": "failed",
                    "iterations": 0,
                    "ok": false,
                })));
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
            python: self.rlm.python.clone(),
            host_fns: self.rlm.host_fns.clone(),
            events: self.rlm.events.clone(),
            depth: next_depth,
            runtime: self.rlm.runtime.clone(),
        };
        let cancel = self.run.cancel();
        let completion = child
            .completion(ContextPayload::Str(prompt), None, &cancel)
            .await;
        self.handler.track_breakdowns(&completion.usage_by_model);
        let result_preview = if completion.response.trim().is_empty() {
            None
        } else {
            Some(truncate_preview(&completion.response, 240))
        };
        let full_result = if completion.response.trim().is_empty() {
            None
        } else {
            Some(completion.response.clone())
        };
        self.rlm.events.emit(RlmEvent::SubcallEnd {
            depth: next_depth,
            model: model_name,
            result_preview,
            result: full_result,
            duration: start.elapsed(),
            error: completion.error.clone(),
        });
        // Build the structured result dict visible to REPL code. 'ok' is true
        // only when the child stopped cleanly; 'stop' uses the flat string form
        // so callers can branch on r['stop'] == 'limit_errors' etc.
        let stop_str = stop_cause_str(&completion.stop);
        let ok = stop_is_ok(&completion.stop) && completion.error.is_none();
        Ok(SubcallResponse::Json(json!({
            "text": completion.response,
            "stop": stop_str,
            "iterations": completion.iterations,
            "ok": ok,
        })))
    }
}

#[async_trait]
impl SubcallHandler for EngineSubcalls {
    fn notify_tool(&self, event: ToolEvent) {
        let depth = self.rlm.depth;
        match event {
            ToolEvent::Start {
                call_id,
                name,
                args,
            } => {
                self.rlm.events.emit(RlmEvent::ToolStart {
                    depth,
                    call_id,
                    name,
                    args,
                });
            }
            ToolEvent::End {
                call_id,
                name,
                result,
                error,
                duration,
            } => {
                self.rlm.events.emit(RlmEvent::ToolEnd {
                    depth,
                    call_id,
                    name,
                    result,
                    error,
                    duration,
                });
            }
        }
    }

    async fn handle(&self, request: SubcallRequest) -> Result<SubcallResponse, String> {
        match request {
            SubcallRequest::Llm { prompt, model } => self.query(prompt, model, "LM").await,
            SubcallRequest::Rlm {
                prompt,
                model,
                preamble,
            } => self.recurse(prompt, model, preamble).await,
            host_fn @ SubcallRequest::HostFn { .. } => match &self.rlm.host_fns {
                Some(handler) => handler.handle(host_fn).await,
                None => {
                    let SubcallRequest::HostFn { name, .. } = host_fn else {
                        unreachable!()
                    };
                    Err(format!("unknown function: {name}"))
                }
            },
            SubcallRequest::ExtTool { name, args } => {
                let input = serde_json::to_string(&args).unwrap_or_default();
                match &self.rlm.runtime {
                    Some(rt) => match rt.invoke_tool_sync(&name, &input) {
                        Ok(Some(output)) => Ok(SubcallResponse::Text(output.content)),
                        Ok(None) => Err(format!("no tool named '{name}'")),
                        Err(e) => Err(format!("tool '{name}' failed: {e}")),
                    },
                    None => Err(format!("no runtime for tool '{name}'")),
                }
            }
        }
    }
}

/// Result of one root-loop run, before env lifecycle and `RunEnd` handling.
pub struct Outcome {
    pub stop: StopCause,
    pub response: String,
    pub error: Option<String>,
    pub iterations: usize,
}

impl Outcome {
    pub fn failed(message: String, iterations: usize) -> Self {
        Self {
            stop: StopCause::Failed,
            response: message.clone(),
            error: Some(message),
            iterations,
        }
    }
}
