//! Max-depth fallback path for [`Rlm`]: passthrough to a bare LM call.

use std::time::Instant;

use phi_ai::Usage;
use serde_json::Value;

use tokio_util::sync::CancellationToken;

use crate::config::{RlmCompletion, RlmEvent, StopCause};
use crate::lm_core::{ChatMessage, ContextPayload, Role};

use super::super::helpers::usage_breakdowns_for_model;
use super::core::Rlm;

impl Rlm {
    pub async fn fallback(
        &self,
        payload: ContextPayload,
        start: Instant,
        cancel: &CancellationToken,
    ) -> RlmCompletion {
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
            summary: None,
            usage,
            usage_by_model,
            iterations: 0,
            duration,
            stop,
            error,
        }
    }
}
