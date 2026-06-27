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
    /// Anthropic content index -> our content index (tool blocks skew them).
    index_map: HashMap<u64, usize>,
    tools: HashMap<u64, ToolBuffer>,
    stop_reason: StopReason,
    errored: bool,
}

impl Parser {
    pub(super) fn new(model: &Model) -> Self {
        Self {
            acc: Accumulator::new(model),
            index_map: HashMap::new(),
            tools: HashMap::new(),
            stop_reason: StopReason::Stop,
            errored: false,
        }
    }

    pub(super) fn finish(self, model: &Model, sink: &EventSink) {
        if self.errored {
            return;
        }
        let mut message = self.acc.into_message();
        message.stop_reason = self.stop_reason;
        message.usage.finalize(&model.cost);
        sink.push(AssistantEvent::Done { message });
    }

    fn block_text(&self, index: usize) -> String {
        match self.acc.message().content.get(index) {
            Some(AssistantContent::Text { text }) => text.clone(),
            Some(AssistantContent::Thinking { thinking, .. }) => thinking.clone(),
            _ => String::new(),
        }
    }
}

fn map_stop_reason(reason: &str) -> StopReason {
    match reason {
        "max_tokens" => StopReason::Length,
        "tool_use" => StopReason::ToolUse,
        "refusal" => StopReason::Error,
        _ => StopReason::Stop,
    }
}

impl SseHandler for Parser {
    fn partial(&self) -> AssistantMessage {
        self.acc.message().clone()
    }

    fn reset(&mut self, model: &Model) {
        self.acc = Accumulator::new(model);
        self.index_map.clear();
        self.tools.clear();
        self.stop_reason = StopReason::Stop;
        self.errored = false;
    }

    fn on_event(&mut self, event_name: &str, data: &str, sink: &EventSink) {
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            return;
        };
        match event_name {
            "message_start" => {
                let usage = &value["message"]["usage"];
                let message = self.acc.message_mut();
                message.usage.input = usage["input_tokens"].as_u64().unwrap_or(0);
                message.usage.cache_read = usage["cache_read_input_tokens"].as_u64().unwrap_or(0);
                message.usage.cache_write =
                    usage["cache_creation_input_tokens"].as_u64().unwrap_or(0);
            }
            "content_block_start" => {
                let Some(index) = value["index"].as_u64() else {
                    return;
                };
                let block = &value["content_block"];
                match block["type"].as_str() {
                    Some("text") => {
                        let ours = self.acc.message().content.len();
                        self.index_map.insert(index, ours);
                        let event = AssistantEvent::TextStart { index: ours };
                        self.acc.apply(&event);
                        sink.push(event);
                    }
                    Some("thinking") => {
                        let ours = self.acc.message().content.len();
                        self.index_map.insert(index, ours);
                        let event = AssistantEvent::ThinkingStart { index: ours };
                        self.acc.apply(&event);
                        sink.push(event);
                    }
                    Some("tool_use") => {
                        let id = block["id"].as_str().unwrap_or_default().to_owned();
                        let name = block["name"].as_str().unwrap_or_default().to_owned();
                        let ours = self.acc.message().content.len();
                        self.index_map.insert(index, ours);
                        self.tools.insert(
                            index,
                            ToolBuffer {
                                id: id.clone(),
                                name: name.clone(),
                                arguments: String::new(),
                            },
                        );
                        let event = AssistantEvent::ToolCallStart {
                            index: ours,
                            id,
                            name,
                        };
                        sink.push(event);
                    }
                    _ => {}
                }
            }
            "content_block_delta" => {
                let Some(index) = value["index"].as_u64() else {
                    return;
                };
                let delta = &value["delta"];
                match delta["type"].as_str() {
                    Some("text_delta") => {
                        let Some(&ours) = self.index_map.get(&index) else {
                            return;
                        };
                        let event = AssistantEvent::TextDelta {
                            index: ours,
                            delta: delta["text"].as_str().unwrap_or_default().to_owned(),
                        };
                        self.acc.apply(&event);
                        sink.push(event);
                    }
                    Some("thinking_delta") => {
                        let Some(&ours) = self.index_map.get(&index) else {
                            return;
                        };
                        let event = AssistantEvent::ThinkingDelta {
                            index: ours,
                            delta: delta["thinking"].as_str().unwrap_or_default().to_owned(),
                        };
                        self.acc.apply(&event);
                        sink.push(event);
                    }
                    Some("signature_delta") => {
                        if let Some(&ours) = self.index_map.get(&index) {
                            self.acc.set_signature(
                                ours,
                                delta["signature"].as_str().unwrap_or_default().to_owned(),
                            );
                        }
                    }
                    Some("input_json_delta") => {
                        let partial = delta["partial_json"].as_str().unwrap_or_default();
                        if let Some(tool) = self.tools.get_mut(&index) {
                            tool.arguments.push_str(partial);
                        }
                        if let Some(&ours) = self.index_map.get(&index) {
                            sink.push(AssistantEvent::ToolCallDelta {
                                index: ours,
                                delta: partial.to_owned(),
                            });
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let Some(index) = value["index"].as_u64() else {
                    return;
                };
                if let Some(tool) = self.tools.remove(&index) {
                    let arguments: serde_json::Value =
                        serde_json::from_str(&tool.arguments).unwrap_or_default();
                    // Resolve our content index; if the index_map is missing
                    // the entry (unexpected), fall back to the current
                    // content length so the ToolCallEnd is still emitted.
                    let ours = self
                        .index_map
                        .get(&index)
                        .copied()
                        .unwrap_or_else(|| self.acc.message().content.len());
                    let event = AssistantEvent::ToolCallEnd {
                        index: ours,
                        id: tool.id,
                        name: tool.name,
                        arguments,
                    };
                    // `apply` inserts the finalized ToolCall into the
                    // accumulator's message content at `index`.
                    self.acc.apply(&event);
                    sink.push(event);
                    return;
                }
                let Some(&ours) = self.index_map.get(&index) else {
                    return;
                };
                let content = self.block_text(ours);
                let event = match self.acc.message().content.get(ours) {
                    Some(AssistantContent::Thinking { .. }) => AssistantEvent::ThinkingEnd {
                        index: ours,
                        content,
                    },
                    _ => AssistantEvent::TextEnd {
                        index: ours,
                        content,
                    },
                };
                sink.push(event);
            }
            "message_delta" => {
                if let Some(reason) = value["delta"]["stop_reason"].as_str() {
                    self.stop_reason = map_stop_reason(reason);
                }
                if let Some(output) = value["usage"]["output_tokens"].as_u64() {
                    self.acc.message_mut().usage.output = output;
                }
            }
            "error" => {
                self.errored = true;
                let detail = value["error"]["message"]
                    .as_str()
                    .unwrap_or("unknown provider error");
                let mut message = self.acc.message().clone();
                message.stop_reason = StopReason::Error;
                message.error_message = Some(detail.to_owned());
                sink.push(AssistantEvent::Error { message });
            }
            _ => {}
        }
    }
}
