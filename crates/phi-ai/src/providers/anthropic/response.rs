use std::collections::HashMap;

use serde::Deserialize;

use crate::providers::SseHandler;
use crate::stream::{Accumulator, AssistantEvent, EventSink};
use crate::types::{AssistantContent, AssistantMessage, Model, StopReason};

// Typed mirrors of the Anthropic SSE event payloads. Deserializing into
// these instead of a generic `serde_json::Value` skips building a JSON
// node tree per event on the hottest path (one `content_block_delta` per
// streamed token batch). Every field defaults so a missing key degrades
// to the same behavior as the old `.as_str().unwrap_or_default()` reads.

#[derive(Deserialize)]
struct MessageStartEvent {
    message: MessageStartBody,
}

#[derive(Deserialize)]
struct MessageStartBody {
    #[serde(default)]
    usage: UsageBody,
}

#[derive(Deserialize, Default)]
struct UsageBody {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
}

#[derive(Deserialize)]
struct ContentBlockStartEvent {
    index: u64,
    content_block: ContentBlockStart,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentBlockStart {
    Text,
    Thinking,
    ToolUse {
        #[serde(default)]
        id: String,
        #[serde(default)]
        name: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct ContentBlockDeltaEvent {
    index: u64,
    delta: BlockDelta,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum BlockDelta {
    TextDelta {
        #[serde(default)]
        text: String,
    },
    ThinkingDelta {
        #[serde(default)]
        thinking: String,
    },
    SignatureDelta {
        #[serde(default)]
        signature: String,
    },
    InputJsonDelta {
        #[serde(default)]
        partial_json: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct ContentBlockStopEvent {
    index: u64,
}

#[derive(Deserialize)]
struct MessageDeltaEvent {
    #[serde(default)]
    delta: MessageDeltaBody,
    #[serde(default)]
    usage: UsageDelta,
}

#[derive(Deserialize, Default)]
struct MessageDeltaBody {
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    stop_details: Option<StopDetails>,
}

/// Detail payload Anthropic attaches to an error-class stop (e.g. a classifier
/// `refusal`), carrying the category and a human-readable explanation.
#[derive(Deserialize, Default)]
struct StopDetails {
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    explanation: Option<String>,
}

#[derive(Deserialize, Default)]
struct UsageDelta {
    #[serde(default)]
    output_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct ErrorEvent {
    #[serde(default)]
    error: ErrorBody,
}

#[derive(Deserialize, Default)]
struct ErrorBody {
    #[serde(default)]
    message: Option<String>,
}

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
    /// Error text for an error-class terminal stop (e.g. a classifier refusal),
    /// surfaced on the terminal message instead of a generic "provider error".
    error_message: Option<String>,
    errored: bool,
}

impl Parser {
    pub(super) fn new(model: &Model) -> Self {
        Self {
            acc: Accumulator::new(model),
            index_map: HashMap::new(),
            tools: HashMap::new(),
            stop_reason: StopReason::Stop,
            error_message: None,
            errored: false,
        }
    }

    pub(super) fn finish(self, model: &Model, sink: &EventSink) {
        if self.errored {
            return;
        }
        let stop_reason = self.stop_reason;
        let error_message = self.error_message;
        let mut message = self.acc.into_message();
        message.stop_reason = stop_reason;
        message.error_message = error_message;
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
        // Classifier-driven terminal stops: `refusal` (e.g. dual-use cyber
        // content) and `sensitive` (safety filter). Both are errors, not a
        // normal end of turn.
        "refusal" | "sensitive" => StopReason::Error,
        _ => StopReason::Stop,
    }
}

/// Build a human-readable error message for an error-class stop, preferring the
/// structured `stop_details` (category + explanation) and falling back to the
/// raw reason. Mirrors ref/oh-my-pi's Anthropic wire handling.
fn refusal_message(reason: &str, details: Option<StopDetails>) -> String {
    if let Some(details) = details
        && details.kind.as_deref() == Some("refusal")
    {
        let label = match details.category {
            Some(cat) if !cat.is_empty() => format!("Refusal ({cat})"),
            _ => "Refusal".to_owned(),
        };
        return match details.explanation.map(|e| e.trim().to_owned()) {
            Some(exp) if !exp.is_empty() => format!("{label}: {exp}"),
            _ => label,
        };
    }
    match reason {
        "refusal" => "Refusal (no details provided)".to_owned(),
        "sensitive" => "Content flagged by safety filters".to_owned(),
        other => format!("Anthropic stream ended with stop_reason: {other}"),
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
        match event_name {
            "message_start" => {
                let Ok(event) = serde_json::from_str::<MessageStartEvent>(data) else {
                    return;
                };
                let usage = event.message.usage;
                let message = self.acc.message_mut();
                message.usage.input = usage.input_tokens;
                message.usage.cache_read = usage.cache_read_input_tokens;
                message.usage.cache_write = usage.cache_creation_input_tokens;
            }
            "content_block_start" => {
                let Ok(event) = serde_json::from_str::<ContentBlockStartEvent>(data) else {
                    return;
                };
                let index = event.index;
                match event.content_block {
                    ContentBlockStart::Text => {
                        let ours = self.acc.message().content.len();
                        self.index_map.insert(index, ours);
                        let event = AssistantEvent::TextStart { index: ours };
                        self.acc.apply(&event);
                        sink.push(event);
                    }
                    ContentBlockStart::Thinking => {
                        let ours = self.acc.message().content.len();
                        self.index_map.insert(index, ours);
                        let event = AssistantEvent::ThinkingStart { index: ours };
                        self.acc.apply(&event);
                        sink.push(event);
                    }
                    ContentBlockStart::ToolUse { id, name } => {
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
                    ContentBlockStart::Other => {}
                }
            }
            "content_block_delta" => {
                let Ok(event) = serde_json::from_str::<ContentBlockDeltaEvent>(data) else {
                    return;
                };
                let index = event.index;
                match event.delta {
                    BlockDelta::TextDelta { text } => {
                        let Some(&ours) = self.index_map.get(&index) else {
                            return;
                        };
                        let event = AssistantEvent::TextDelta {
                            index: ours,
                            delta: text,
                        };
                        self.acc.apply(&event);
                        sink.push(event);
                    }
                    BlockDelta::ThinkingDelta { thinking } => {
                        let Some(&ours) = self.index_map.get(&index) else {
                            return;
                        };
                        let event = AssistantEvent::ThinkingDelta {
                            index: ours,
                            delta: thinking,
                        };
                        self.acc.apply(&event);
                        sink.push(event);
                    }
                    BlockDelta::SignatureDelta { signature } => {
                        if let Some(&ours) = self.index_map.get(&index) {
                            self.acc.set_signature(ours, signature);
                        }
                    }
                    BlockDelta::InputJsonDelta { partial_json } => {
                        if let Some(tool) = self.tools.get_mut(&index) {
                            tool.arguments.push_str(&partial_json);
                        }
                        if let Some(&ours) = self.index_map.get(&index) {
                            sink.push(AssistantEvent::ToolCallDelta {
                                index: ours,
                                delta: partial_json,
                            });
                        }
                    }
                    BlockDelta::Other => {}
                }
            }
            "content_block_stop" => {
                let Ok(event) = serde_json::from_str::<ContentBlockStopEvent>(data) else {
                    return;
                };
                let index = event.index;
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
                let Ok(event) = serde_json::from_str::<MessageDeltaEvent>(data) else {
                    return;
                };
                if let Some(reason) = event.delta.stop_reason {
                    self.stop_reason = map_stop_reason(&reason);
                    if self.stop_reason == StopReason::Error {
                        self.error_message =
                            Some(refusal_message(&reason, event.delta.stop_details));
                    }
                }
                if let Some(output) = event.usage.output_tokens {
                    self.acc.message_mut().usage.output = output;
                }
            }
            "error" => {
                let Ok(event) = serde_json::from_str::<ErrorEvent>(data) else {
                    return;
                };
                self.errored = true;
                let detail = event
                    .error
                    .message
                    .unwrap_or_else(|| "unknown provider error".to_owned());
                let mut message = self.acc.message().clone();
                message.stop_reason = StopReason::Error;
                message.error_message = Some(detail);
                sink.push(AssistantEvent::Error { message });
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::AssistantEventStream;
    use crate::types::{Api, CostRates, Model, ToolCall};

    fn model() -> Model {
        Model {
            id: "claude-test".to_owned(),
            name: "Test".to_owned(),
            api: Api::AnthropicMessages,
            provider: "anthropic".to_owned(),
            base_url: "https://example.test".to_owned(),
            reasoning: false,
            input: vec![],
            cost: CostRates::default(),
            context_window: 1000,
            max_tokens: 100,
            headers: Default::default(),
            thinking_levels: None,
            adaptive_thinking: false,
        }
    }

    #[tokio::test]
    async fn parses_typed_anthropic_events() {
        let model = model();
        let mut parser = Parser::new(&model);
        let (sink, mut stream) = AssistantEventStream::channel();

        parser.on_event(
            "message_start",
            r#"{"type":"message_start","message":{"usage":{"input_tokens":7,"cache_read_input_tokens":3,"cache_creation_input_tokens":2}}}"#,
            &sink,
        );
        parser.on_event(
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            &sink,
        );
        parser.on_event(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello"}}"#,
            &sink,
        );
        parser.on_event(
            "content_block_stop",
            r#"{"type":"content_block_stop","index":0}"#,
            &sink,
        );
        parser.on_event(
            "content_block_start",
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"t1","name":"run"}}"#,
            &sink,
        );
        parser.on_event(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"x\":1}"}}"#,
            &sink,
        );
        parser.on_event(
            "content_block_stop",
            r#"{"type":"content_block_stop","index":1}"#,
            &sink,
        );
        // Unknown block/delta kinds and malformed payloads must be skipped,
        // not fail the stream.
        parser.on_event(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"new_delta_kind"}}"#,
            &sink,
        );
        parser.on_event("message_delta", "not json", &sink);
        parser.on_event(
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":9}}"#,
            &sink,
        );
        parser.finish(&model, &sink);
        drop(sink);

        let mut done = None;
        while let Some(event) = stream.next().await {
            if let AssistantEvent::Done { message } = event {
                done = Some(message);
            }
        }
        let message = done.expect("terminal Done event");
        assert_eq!(message.text(), "hello");
        assert_eq!(message.stop_reason, StopReason::ToolUse);
        assert_eq!(message.usage.input, 7);
        assert_eq!(message.usage.output, 9);
        assert_eq!(message.usage.cache_read, 3);
        assert_eq!(message.usage.cache_write, 2);
        let tool: &ToolCall = message
            .content
            .iter()
            .find_map(|block| match block {
                AssistantContent::ToolCall(tool) => Some(tool),
                _ => None,
            })
            .expect("tool call present");
        assert_eq!(tool.id, "t1");
        assert_eq!(tool.name, "run");
        assert_eq!(tool.arguments["x"], 1);
    }

    #[tokio::test]
    async fn refusal_stop_surfaces_detailed_error() {
        let model = model();
        let mut parser = Parser::new(&model);
        let (sink, mut stream) = AssistantEventStream::channel();

        parser.on_event(
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"refusal","stop_details":{"type":"refusal","category":"cyber","explanation":"This request triggered restrictions on violative cyber content."}},"usage":{"output_tokens":0}}"#,
            &sink,
        );
        parser.finish(&model, &sink);
        drop(sink);

        let mut done = None;
        while let Some(event) = stream.next().await {
            if let AssistantEvent::Done { message } = event {
                done = Some(message);
            }
        }
        let message = done.expect("terminal Done event");
        assert_eq!(message.stop_reason, StopReason::Error);
        assert!(message.is_error());
        assert_eq!(
            message.error_message.as_deref(),
            Some(
                "Refusal (cyber): This request triggered restrictions on violative cyber content."
            )
        );
    }

    #[test]
    fn refusal_message_falls_back_without_details() {
        assert_eq!(
            refusal_message("refusal", None),
            "Refusal (no details provided)"
        );
        assert_eq!(
            refusal_message("sensitive", None),
            "Content flagged by safety filters"
        );
        // Category present, explanation missing → label only.
        let details = StopDetails {
            kind: Some("refusal".to_owned()),
            category: Some("cyber".to_owned()),
            explanation: None,
        };
        assert_eq!(refusal_message("refusal", Some(details)), "Refusal (cyber)");
    }

    #[test]
    fn sensitive_stop_reason_maps_to_error() {
        assert_eq!(map_stop_reason("sensitive"), StopReason::Error);
        assert_eq!(map_stop_reason("refusal"), StopReason::Error);
        assert_eq!(map_stop_reason("end_turn"), StopReason::Stop);
    }
}
