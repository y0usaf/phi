//! Streaming sinks for [`Rlm`]: delta, thinking, and tool-call channels.

use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;

use crate::config::RlmEvent;
use crate::lm_core::{DeltaSink, ThinkingSink, ToolCallEvent, ToolCallSink};

use super::core::{Rlm, ToolCallResult};

impl Rlm {
    pub(super) fn delta_sink(&self, iteration: usize) -> DeltaSink {
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

    pub(super) fn thinking_sink(&self, iteration: usize) -> ThinkingSink {
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

    /// Tool-call sink that executes code eagerly during the stream.
    pub(super) fn tool_call_sink(
        &self,
        _iteration: usize,
        tool_tx: UnboundedSender<ToolCallResult>,
    ) -> ToolCallSink {
        let tx = tool_tx.clone();
        Arc::new(move |event: ToolCallEvent| {
            if let ToolCallEvent::End { id, arguments, .. } = &event {
                let code = arguments
                    .get("code")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_owned());
                let _ = tx.send(ToolCallResult {
                    id: id.clone(),
                    code,
                });
            }
        })
    }
}
