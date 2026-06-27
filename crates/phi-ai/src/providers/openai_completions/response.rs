use std::collections::HashMap;

use serde_json::Value;

use crate::providers::SseHandler;
use crate::stream::{Accumulator, AssistantEvent, EventSink};
use crate::types::{AssistantContent, AssistantMessage, Model, StopReason};

struct ToolBuffer {
    id: String,
    name: String,
    arguments: String,
}

pub(super) struct Parser {
    acc: Accumulator,
    /// Tool call index (from delta.tool_calls[].index) -> buffer.
    tools: HashMap<u64, ToolBuffer>,
    stop_reason: StopReason,
    errored: bool,
    tool_use_seen: bool,
}

impl Parser {
    pub(super) fn new(model: &Model) -> Self {
        Self {
            acc: Accumulator::new(model),
            tools: HashMap::new(),
            stop_reason: StopReason::Stop,
            errored: false,
            tool_use_seen: false,
        }
    }

    pub(super) fn finish(mut self, model: &Model, sink: &EventSink) {
        if self.errored {
            return;
        }
        // Emit ToolCallEnd for each buffered tool call before the final message.
        // OpenAI Completions doesn't have a per-call done event, so we finalize
        // all buffered calls here.
        let mut sorted_tools: Vec<(u64, ToolBuffer)> = self.tools.into_iter().collect();
        sorted_tools.sort_by_key(|(idx, _)| *idx);
        for (idx, tool) in sorted_tools {
            let arguments: serde_json::Value =
                serde_json::from_str(&tool.arguments).unwrap_or_default();
            let call = crate::types::ToolCall {
                id: tool.id.clone(),
                name: tool.name.clone(),
                arguments: arguments.clone(),
            };
            let ours = idx as usize;
            // Ensure the tool call is in the accumulator's message.
            self.acc.push_tool_call(call);
            let event = AssistantEvent::ToolCallEnd {
                index: ours,
                id: tool.id,
                name: tool.name,
                arguments,
            };
            sink.push(event);
        }
        let mut message = self.acc.into_message();
        if self.tool_use_seen && self.stop_reason == StopReason::Stop {
            self.stop_reason = StopReason::ToolUse;
        }
        message.stop_reason = self.stop_reason;
        message.usage.finalize(&model.cost);
        sink.push(AssistantEvent::Done { message });
    }

    fn apply_usage(&mut self, usage: &Value) {
        if usage.is_null() {
            return;
        }
        let prompt_tokens = usage["prompt_tokens"].as_u64().unwrap_or(0);
        let completion_tokens = usage["completion_tokens"].as_u64().unwrap_or(0);
        let cache_read = usage["prompt_tokens_details"]["cached_tokens"]
            .as_u64()
            .unwrap_or(0);

        let message = self.acc.message_mut();
        message.usage.cache_read = cache_read;
        message.usage.input = prompt_tokens.saturating_sub(cache_read);
        message.usage.output = completion_tokens;
    }
}

fn map_stop_reason(reason: &str) -> (StopReason, Option<String>) {
    match reason {
        "stop" | "end" | "" => (StopReason::Stop, None),
        "length" => (StopReason::Length, None),
        "function_call" | "tool_calls" => (StopReason::ToolUse, None),
        "content_filter" => (
            StopReason::Error,
            Some("Provider finish_reason: content_filter".to_owned()),
        ),
        other => (
            StopReason::Error,
            Some(format!("Provider finish_reason: {other}")),
        ),
    }
}

impl SseHandler for Parser {
    fn partial(&self) -> AssistantMessage {
        self.acc.message().clone()
    }

    fn reset(&mut self, model: &Model) {
        self.acc = Accumulator::new(model);
        self.tools.clear();
        self.stop_reason = StopReason::Stop;
        self.errored = false;
        self.tool_use_seen = false;
    }

    fn on_event(&mut self, _event_name: &str, data: &str, sink: &EventSink) {
        // OpenAI sends "[DONE]" as the terminal marker.
        if data == "[DONE]" {
            return;
        }
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            return;
        };

        // Error object (some providers inline it in the stream).
        if value.get("error").is_some() {
            self.errored = true;
            let detail = value["error"]["message"]
                .as_str()
                .unwrap_or("unknown provider error");
            let mut message = self.acc.message().clone();
            message.stop_reason = StopReason::Error;
            message.error_message = Some(detail.to_owned());
            sink.push(AssistantEvent::Error { message });
            return;
        }

        // Usage may arrive in a chunk with no choices (the final chunk when
        // stream_options.include_usage is true).
        if let Some(usage) = value.get("usage").filter(|u| !u.is_null()) {
            self.apply_usage(usage);
        }

        let Some(choices) = value["choices"].as_array() else {
            return;
        };
        let Some(choice) = choices.first() else {
            return;
        };

        if let Some(finish) = choice["finish_reason"].as_str().filter(|f| !f.is_empty()) {
            let (reason, error) = map_stop_reason(finish);
            self.stop_reason = reason;
            if let Some(msg) = error {
                self.errored = true;
                let mut message = self.acc.message().clone();
                message.stop_reason = StopReason::Error;
                message.error_message = Some(msg);
                sink.push(AssistantEvent::Error { message });
                return;
            }
        }

        let Some(delta) = choice.get("delta") else {
            return;
        };

        // Text content.
        if let Some(content) = delta["content"].as_str().filter(|c| !c.is_empty()) {
            let index = self
                .acc
                .message()
                .content
                .iter()
                .position(|b| matches!(b, AssistantContent::Text { .. }))
                .unwrap_or_else(|| self.acc.message().content.len());
            if !self
                .acc
                .message()
                .content
                .get(index)
                .is_some_and(|b| matches!(b, AssistantContent::Text { .. }))
            {
                let event = AssistantEvent::TextStart { index };
                self.acc.apply(&event);
                sink.push(event);
            }
            let event = AssistantEvent::TextDelta {
                index,
                delta: content.to_owned(),
            };
            self.acc.apply(&event);
            sink.push(event);
        }

        // Reasoning content (non-standard but widely used: reasoning_content,
        // reasoning, reasoning_text). Use the first non-empty field.
        for field in ["reasoning_content", "reasoning", "reasoning_text"] {
            if let Some(thinking) = delta
                .get(field)
                .and_then(|v| v.as_str())
                .filter(|t| !t.is_empty())
            {
                let index = self
                    .acc
                    .message()
                    .content
                    .iter()
                    .position(|b| matches!(b, AssistantContent::Thinking { .. }))
                    .unwrap_or_else(|| self.acc.message().content.len());
                if !self
                    .acc
                    .message()
                    .content
                    .get(index)
                    .is_some_and(|b| matches!(b, AssistantContent::Thinking { .. }))
                {
                    let event = AssistantEvent::ThinkingStart { index };
                    self.acc.apply(&event);
                    sink.push(event);
                }
                let event = AssistantEvent::ThinkingDelta {
                    index,
                    delta: thinking.to_owned(),
                };
                self.acc.apply(&event);
                sink.push(event);
                break;
            }
        }

        // Tool calls.
        if let Some(tool_calls) = delta["tool_calls"].as_array() {
            for tc in tool_calls {
                let Some(idx) = tc["index"].as_u64() else {
                    continue;
                };
                let is_new = !self.tools.contains_key(&idx);
                let entry = self.tools.entry(idx).or_insert_with(|| ToolBuffer {
                    id: String::new(),
                    name: String::new(),
                    arguments: String::new(),
                });
                if let Some(id) = tc["id"].as_str() {
                    entry.id = id.to_owned();
                }
                if let Some(name) = tc["function"]["name"].as_str() {
                    entry.name = name.to_owned();
                }
                if let Some(args) = tc["function"]["arguments"].as_str() {
                    entry.arguments.push_str(args);
                }
                self.tool_use_seen = true;

                // Emit streaming events.
                let ours = idx as usize;
                if is_new {
                    sink.push(AssistantEvent::ToolCallStart {
                        index: ours,
                        id: entry.id.clone(),
                        name: entry.name.clone(),
                    });
                }
                if let Some(args) = tc["function"]["arguments"].as_str()
                    && !args.is_empty()
                {
                    sink.push(AssistantEvent::ToolCallDelta {
                        index: ours,
                        delta: args.to_owned(),
                    });
                }
            }
        }
    }
}
