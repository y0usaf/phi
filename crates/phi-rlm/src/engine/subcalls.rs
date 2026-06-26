//! Subcall routing: `llm_query` to the sub model, `rlm_query` to a child
//! RLM, host fns to the registered handler.

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use phi_repl::{SubcallHandler, SubcallRequest, SubcallResponse, ToolEvent};

use phi_lm_core::LmHandler;
use phi_lm_core::ContextPayload;

use crate::config::{RlmEvent, StopCause};
use super::helpers::{prompt_preview, truncate_preview};
use super::rlm::Rlm;
use super::state::RunState;

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
        let result = self.handler.complete_sub(&prompt, model.as_deref(), &cancel).await;
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

