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
    /// Responses item id -> our content index (for text/thinking blocks).
    items: HashMap<String, usize>,
    /// Responses item id -> our content index (for tool-call blocks).
    /// Kept separate from `items` because tool calls occupy their own
    /// content-block slots and are looked up by call_id, not item_id.
    tool_index_map: HashMap<String, usize>,
    /// Responses item id -> tool call buffer (function_call items).
    tools: HashMap<String, ToolBuffer>,
    stop_reason: StopReason,
    usage_seen: bool,
    errored: bool,
    tool_use_seen: bool,
}

impl Parser {
    pub(super) fn new(model: &Model) -> Self {
        Self {
            acc: Accumulator::new(model),
            items: HashMap::new(),
            tool_index_map: HashMap::new(),
            tools: HashMap::new(),
            stop_reason: StopReason::Stop,
            usage_seen: false,
            errored: false,
            tool_use_seen: false,
        }
    }

    pub(super) fn finish(mut self, model: &Model, sink: &EventSink) {
        if self.errored {
            return;
        }
        let mut message = self.acc.into_message();
        if self.tool_use_seen && self.stop_reason == StopReason::Stop {
            self.stop_reason = StopReason::ToolUse;
        }
        message.stop_reason = self.stop_reason;
        message.usage.finalize(&model.cost);
        sink.push(AssistantEvent::Done { message });
    }

    fn item_index(&mut self, item_id: &str, thinking: bool) -> usize {
        if let Some(&index) = self.items.get(item_id) {
            return index;
        }
        let index = self.acc.message().content.len();
        self.items.insert(item_id.to_owned(), index);
        let event = if thinking {
            AssistantEvent::ThinkingStart { index }
        } else {
            AssistantEvent::TextStart { index }
        };
        self.acc.apply(&event);
        index
    }

    fn apply_usage(&mut self, usage: &Value) {
        if usage.is_null() {
            return;
        }
        self.usage_seen = true;
        let cached = usage["input_tokens_details"]["cached_tokens"]
            .as_u64()
            .unwrap_or(0);
        let input = usage["input_tokens"].as_u64().unwrap_or(0);
        let message = self.acc.message_mut();
        message.usage.cache_read = cached;
        message.usage.input = input.saturating_sub(cached);
        message.usage.output = usage["output_tokens"].as_u64().unwrap_or(0);
    }

    fn emit_error(&mut self, detail: &str, sink: &EventSink) {
        self.errored = true;
        let mut message = self.acc.message().clone();
        message.stop_reason = StopReason::Error;
        message.error_message = Some(detail.to_owned());
        sink.push(AssistantEvent::Error { message });
    }
}

impl SseHandler for Parser {
    fn partial(&self) -> AssistantMessage {
        self.acc.message().clone()
    }

    fn reset(&mut self, model: &Model) {
        self.acc = Accumulator::new(model);
        self.items.clear();
        self.tool_index_map.clear();
        self.tools.clear();
        self.stop_reason = StopReason::Stop;
        self.usage_seen = false;
        self.errored = false;
        self.tool_use_seen = false;
    }

    fn on_event(&mut self, _event_name: &str, data: &str, sink: &EventSink) {
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            return;
        };
        // Responses encodes the event type in the payload.
        match value["type"].as_str().unwrap_or_default() {
            "response.output_text.delta" => {
                let item_id = value["item_id"].as_str().unwrap_or_default().to_owned();
                let index = self.item_index(&item_id, false);
                let event = AssistantEvent::TextDelta {
                    index,
                    delta: value["delta"].as_str().unwrap_or_default().to_owned(),
                };
                self.acc.apply(&event);
                sink.push(event);
            }
            "response.output_text.done" => {
                let item_id = value["item_id"].as_str().unwrap_or_default().to_owned();
                let index = self.item_index(&item_id, false);
                let content = value["text"].as_str().unwrap_or_default().to_owned();
                let event = AssistantEvent::TextEnd { index, content };
                self.acc.apply(&event);
                sink.push(event);
            }
            "response.reasoning_summary_text.delta" => {
                let item_id = value["item_id"].as_str().unwrap_or_default().to_owned();
                let index = self.item_index(&item_id, true);
                let event = AssistantEvent::ThinkingDelta {
                    index,
                    delta: value["delta"].as_str().unwrap_or_default().to_owned(),
                };
                self.acc.apply(&event);
                sink.push(event);
            }
            "response.reasoning_summary_part.added" => {
                // Separate summary parts within one reasoning item with a blank line.
                let item_id = value["item_id"].as_str().unwrap_or_default().to_owned();
                let index = self.item_index(&item_id, true);
                if let Some(AssistantContent::Thinking { thinking, .. }) =
                    self.acc.message().content.get(index)
                    && !thinking.is_empty()
                {
                    let event = AssistantEvent::ThinkingDelta {
                        index,
                        delta: "\n\n".to_owned(),
                    };
                    self.acc.apply(&event);
                    sink.push(event);
                }
            }
            "response.completed" | "response.incomplete" => {
                let response = &value["response"];
                self.apply_usage(&response["usage"]);
                let incomplete_reason = response["incomplete_details"]["reason"]
                    .as_str()
                    .unwrap_or_default();
                if response["status"].as_str() == Some("incomplete")
                    && incomplete_reason == "max_output_tokens"
                {
                    self.stop_reason = StopReason::Length;
                }
            }
            "response.failed" => {
                let response = &value["response"];
                self.apply_usage(&response["usage"]);
                let detail = response["error"]["message"]
                    .as_str()
                    .unwrap_or("response failed");
                self.emit_error(detail, sink);
            }
            "error" => {
                let detail = value["message"]
                    .as_str()
                    .unwrap_or("unknown provider error");
                self.emit_error(detail, sink);
            }
            // Tool call: a new function_call output item is being created.
            "response.output_item.added" => {
                let item = &value["item"];
                if item["type"].as_str() == Some("function_call") {
                    let item_id = item["call_id"]
                        .as_str()
                        .or_else(|| item["id"].as_str())
                        .unwrap_or_default()
                        .to_owned();
                    let name = item["name"].as_str().unwrap_or_default().to_owned();
                    // Arguments may arrive in full on this event or stream in
                    // via the `.delta` events below; buffer either way.
                    let arguments = item["arguments"].as_str().unwrap_or_default().to_owned();
                    let ours = self.acc.message().content.len();
                    // Record the content-block index for this tool call so
                    // later delta/done events can find it without scanning.
                    self.tool_index_map.insert(item_id.clone(), ours);
                    self.tools.insert(
                        item_id.clone(),
                        ToolBuffer {
                            id: item_id.clone(),
                            name: name.clone(),
                            arguments,
                        },
                    );
                    self.tool_use_seen = true;
                    sink.push(AssistantEvent::ToolCallStart {
                        index: ours,
                        id: item_id,
                        name,
                    });
                }
            }
            // Tool call: streaming argument deltas.
            "response.function_call_arguments.delta" => {
                let item_id = value["item_id"]
                    .as_str()
                    .or_else(|| value["call_id"].as_str())
                    .unwrap_or_default()
                    .to_owned();
                let delta = value["delta"].as_str().unwrap_or_default().to_owned();
                if let Some(tool) = self.tools.get_mut(&item_id) {
                    tool.arguments.push_str(&delta);
                }
                // Use the recorded content-block index from tool_index_map
                // (set in output_item.added) rather than scanning the
                // accumulator — the tool call may not be in the accumulator
                // yet, so the old `content.len().saturating_sub(1)` fallback
                // could attach the delta to the wrong block.
                if let Some(&ours) = self.tool_index_map.get(&item_id) {
                    sink.push(AssistantEvent::ToolCallDelta { index: ours, delta });
                }
            }
            // Tool call: arguments done — finalize the tool call.
            "response.function_call_arguments.done" => {
                let item_id = value["item_id"]
                    .as_str()
                    .or_else(|| value["call_id"].as_str())
                    .unwrap_or_default()
                    .to_owned();
                // If the full arguments string is provided, replace the buffer.
                if let Some(args) = value["arguments"].as_str()
                    && let Some(tool) = self.tools.get_mut(&item_id)
                {
                    tool.arguments = args.to_owned();
                }
            }
            // Tool call: the output item is complete — push it to the message.
            "response.output_item.done" => {
                let item = &value["item"];
                if item["type"].as_str() == Some("function_call") {
                    let item_id = item["call_id"]
                        .as_str()
                        .or_else(|| item["id"].as_str())
                        .unwrap_or_default()
                        .to_owned();
                    if let Some(tool) = self.tools.remove(&item_id) {
                        let arguments: serde_json::Value =
                            serde_json::from_str(&tool.arguments).unwrap_or_default();
                        let call = crate::types::ToolCall {
                            id: tool.id.clone(),
                            name: tool.name.clone(),
                            arguments: arguments.clone(),
                        };
                        // Use the recorded content-block index from
                        // tool_index_map (set in output_item.added).
                        let ours = self
                            .tool_index_map
                            .get(&item_id)
                            .copied()
                            .unwrap_or_else(|| self.acc.message().content.len());
                        let event = AssistantEvent::ToolCallEnd {
                            index: ours,
                            id: tool.id,
                            name: tool.name,
                            arguments,
                        };
                        self.acc.push_tool_call(call);
                        sink.push(event);
                    }
                }
            }
            _ => {}
        }
    }
}
