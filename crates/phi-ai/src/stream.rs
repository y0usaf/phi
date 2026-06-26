use tokio::sync::mpsc;

use crate::types::{AssistantContent, AssistantMessage, Model, StopReason, ToolCall};

#[derive(Clone, Debug)]
pub enum AssistantEvent {
    Start,
    TextStart { index: usize },
    TextDelta { index: usize, delta: String },
    TextEnd { index: usize, content: String },
    ThinkingStart { index: usize },
    ThinkingDelta { index: usize, delta: String },
    ThinkingEnd { index: usize, content: String },
    /// The model started a tool call; `index` is the content-block index.
    ToolCallStart {
        index: usize,
        id: String,
        name: String,
    },
    /// Streaming partial JSON for a tool call's arguments.
    ToolCallDelta {
        index: usize,
        delta: String,
    },
    /// A tool call's arguments are fully assembled.
    ToolCallEnd {
        index: usize,
        id: String,
        name: String,
        arguments: serde_json::Value,
    },
    Done { message: AssistantMessage },
    Error { message: AssistantMessage },
    /// Sent before a mid-stream retry; the consumer should discard partial
    /// output accumulated since the last `Start`/`RetryRollback`.
    RetryRollback { attempt: u32, max: u32 },
    /// Keepalive signal emitted periodically during slow generation so
    /// consumers know the stream is still alive.
    Pong,
}

impl AssistantEvent {
    pub fn terminal_message(&self) -> Option<&AssistantMessage> {
        match self {
            Self::Done { message } | Self::Error { message } => Some(message),
            _ => None,
        }
    }
}

/// Push-based event stream. The producer side never errors out-of-band:
/// failures arrive as a terminal `Error` event.
pub struct AssistantEventStream {
    rx: mpsc::UnboundedReceiver<AssistantEvent>,
}

impl AssistantEventStream {
    pub fn channel() -> (EventSink, Self) {
        let (tx, rx) = mpsc::unbounded_channel();
        (EventSink { tx }, Self { rx })
    }

    pub async fn next(&mut self) -> Option<AssistantEvent> {
        self.rx.recv().await
    }

    /// Drain the stream and return the terminal message.
    pub async fn result(mut self) -> AssistantMessage {
        let mut last = None;
        while let Some(event) = self.next().await {
            if let Some(message) = event.terminal_message() {
                last = Some(message.clone());
            }
        }
        last.unwrap_or_else(|| AssistantMessage {
            content: Vec::new(),
            api: crate::types::Api::AnthropicMessages,
            provider: String::new(),
            model: String::new(),
            usage: Default::default(),
            stop_reason: StopReason::Error,
            error_message: Some("stream ended without a terminal event".to_owned()),
            timestamp: crate::types::now_ms(),
        })
    }
}

#[derive(Clone)]
pub struct EventSink {
    tx: mpsc::UnboundedSender<AssistantEvent>,
}

impl EventSink {
    pub fn push(&self, event: AssistantEvent) {
        let _ = self.tx.send(event);
    }

    pub fn error(&self, model: &Model, stop_reason: StopReason, error: impl Into<String>) {
        self.push(AssistantEvent::Error {
            message: AssistantMessage::error(model, stop_reason, error),
        });
    }
}

/// Folds delta events into an `AssistantMessage` (providers build their final
/// message with this; frontends can use it for live partials).
#[derive(Debug)]
pub struct Accumulator {
    message: AssistantMessage,
}

impl Accumulator {
    pub fn new(model: &Model) -> Self {
        Self {
            message: AssistantMessage::empty(model),
        }
    }

    pub fn message(&self) -> &AssistantMessage {
        &self.message
    }

    pub fn into_message(self) -> AssistantMessage {
        self.message
    }

    fn ensure_index(&mut self, index: usize, thinking: bool) -> &mut AssistantContent {
        while self.message.content.len() <= index {
            self.message.content.push(if thinking {
                AssistantContent::Thinking {
                    thinking: String::new(),
                    signature: None,
                }
            } else {
                AssistantContent::Text {
                    text: String::new(),
                }
            });
        }
        &mut self.message.content[index]
    }

    pub fn push_tool_call(&mut self, call: ToolCall) {
        self.message.content.push(AssistantContent::ToolCall(call));
    }

    pub fn set_signature(&mut self, index: usize, sig: String) {
        if let Some(AssistantContent::Thinking { signature, .. }) =
            self.message.content.get_mut(index)
        {
            *signature = Some(sig);
        }
    }

    pub fn apply(&mut self, event: &AssistantEvent) {
        match event {
            AssistantEvent::TextStart { index } => {
                self.ensure_index(*index, false);
            }
            AssistantEvent::TextDelta { index, delta } => {
                if let AssistantContent::Text { text } = self.ensure_index(*index, false) {
                    text.push_str(delta);
                }
            }
            AssistantEvent::TextEnd { index, content } => {
                if let AssistantContent::Text { text } = self.ensure_index(*index, false) {
                    *text = content.clone();
                }
            }
            AssistantEvent::ThinkingStart { index } => {
                self.ensure_index(*index, true);
            }
            AssistantEvent::ThinkingDelta { index, delta } => {
                if let AssistantContent::Thinking { thinking, .. } = self.ensure_index(*index, true)
                {
                    thinking.push_str(delta);
                }
            }
            AssistantEvent::ThinkingEnd { index, content } => {
                if let AssistantContent::Thinking { thinking, .. } = self.ensure_index(*index, true)
                {
                    *thinking = content.clone();
                }
            }
            AssistantEvent::ToolCallStart { index, .. } => {
                // Ensure the content block exists; the provider's
                // `push_tool_call` finalizes it at `content_block_stop`.
                while self.message.content.len() <= *index {
                    self.message.content.push(AssistantContent::Text {
                        text: String::new(),
                    });
                }
            }
            AssistantEvent::ToolCallEnd {
                index,
                id,
                name,
                arguments,
            } => {
                // Replace or insert the finalized tool call at `index`.
                let call = ToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    arguments: arguments.clone(),
                };
                if *index < self.message.content.len() {
                    self.message.content[*index] = AssistantContent::ToolCall(call);
                } else {
                    while self.message.content.len() < *index {
                        self.message.content.push(AssistantContent::Text {
                            text: String::new(),
                        });
                    }
                    self.message.content.push(AssistantContent::ToolCall(call));
                }
            }
            AssistantEvent::Start
            | AssistantEvent::Done { .. }
            | AssistantEvent::Error { .. }
            | AssistantEvent::RetryRollback { .. }
            | AssistantEvent::Pong
            | AssistantEvent::ToolCallDelta { .. } => {}
        }
    }

    pub fn message_mut(&mut self) -> &mut AssistantMessage {
        &mut self.message
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Api, CostRates, Model};

    fn test_model() -> Model {
        Model {
            id: "m".into(),
            name: "M".into(),
            api: Api::AnthropicMessages,
            provider: "anthropic".into(),
            base_url: "https://example.test".into(),
            reasoning: false,
            input: vec![],
            cost: CostRates::default(),
            context_window: 1000,
            max_tokens: 100,
            headers: Default::default(),
            thinking_levels: None,
        }
    }

    #[tokio::test]
    async fn stream_result_returns_terminal_message() {
        let model = test_model();
        let (sink, stream) = AssistantEventStream::channel();
        let mut acc = Accumulator::new(&model);

        for event in [
            AssistantEvent::Start,
            AssistantEvent::TextStart { index: 0 },
            AssistantEvent::TextDelta {
                index: 0,
                delta: "hel".into(),
            },
            AssistantEvent::TextDelta {
                index: 0,
                delta: "lo".into(),
            },
        ] {
            acc.apply(&event);
            sink.push(event);
        }
        sink.push(AssistantEvent::Done {
            message: acc.into_message(),
        });
        drop(sink);

        let message = stream.result().await;
        assert_eq!(message.text(), "hello");
        assert_eq!(message.stop_reason, StopReason::Stop);
    }

    #[tokio::test]
    async fn closed_stream_without_terminal_synthesizes_error() {
        let (sink, stream) = AssistantEventStream::channel();
        sink.push(AssistantEvent::Start);
        drop(sink);
        let message = stream.result().await;
        assert_eq!(message.stop_reason, StopReason::Error);
    }

    #[test]
    fn usage_cost_math() {
        let mut usage = crate::types::Usage {
            input: 1_000_000,
            output: 500_000,
            cache_read: 2_000_000,
            cache_write: 0,
            ..Default::default()
        };
        usage.finalize(&CostRates {
            input: 3.0,
            output: 15.0,
            cache_read: 0.3,
            cache_write: 3.75,
        });
        assert!((usage.cost.total - (3.0 + 7.5 + 0.6)).abs() < 1e-9);
        assert_eq!(usage.total_tokens, 3_500_000);
    }
}
