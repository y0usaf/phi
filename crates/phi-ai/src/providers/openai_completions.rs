//! OpenAI Chat Completions API provider (SSE streaming).
//!
//! Standard OpenAI-compatible `/chat/completions` endpoint with `stream: true`
//! and `stream_options: { include_usage: true }`. Works with any provider that
//! implements the Chat Completions API (Neuralwatt, 302ai, OpenRouter, etc.).

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::providers::{SseHandler, http_client, run_sse};
use crate::registry::ApiProvider;
use crate::stream::{Accumulator, AssistantEvent, AssistantEventStream, EventSink};
use crate::types::{
    Api, AssistantContent, AssistantMessage, Context, Message, Model, StopReason,
    StreamOptions, ToolDefinition,
};

pub struct OpenAiCompletionsProvider {
    client: reqwest::Client,
}

impl OpenAiCompletionsProvider {
    pub fn new() -> Self {
        Self {
            client: http_client(),
        }
    }
}

impl Default for OpenAiCompletionsProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl ApiProvider for OpenAiCompletionsProvider {
    fn api(&self) -> Api {
        Api::OpenAiCompletions
    }

    fn stream(&self, model: &Model, ctx: &Context, opts: StreamOptions) -> AssistantEventStream {
        let (sink, stream) = AssistantEventStream::channel();
        let client = self.client.clone();
        let model = model.clone();
        let ctx = ctx.clone();
        tokio::spawn(async move {
            run(client, model, ctx, opts, sink).await;
        });
        stream
    }
}

async fn run(
    client: reqwest::Client,
    model: Model,
    ctx: Context,
    opts: StreamOptions,
    sink: EventSink,
) {
    let body = build_body(&model, &ctx, &opts);
    let url = endpoint_url(&model);
    let mut handler = Parser::new(&model);

    let build_request = |client: &reqwest::Client| {
        let mut request = client
            .post(&url)
            .bearer_auth(opts.auth.secret())
            .header("accept", "text/event-stream");
        for (name, value) in &model.headers {
            request = request.header(name, value);
        }
        request.json(&body)
    };

    if run_sse(&client, build_request, &model, &opts, &sink, &mut handler).await {
        handler.finish(&model, &sink);
    }
}

fn endpoint_url(model: &Model) -> String {
    format!("{}/chat/completions", model.base_url.trim_end_matches('/'))
}

fn build_body(model: &Model, ctx: &Context, opts: &StreamOptions) -> Value {
    let mut body = json!({
        "model": model.id,
        "stream": true,
        "stream_options": { "include_usage": true },
        "messages": convert_messages(&ctx.messages, ctx.system_prompt.as_deref()),
    });

    // Cap to context_window defensively: some upstream model listings
    // (notably neuralwatt) advertise `output` limits that exceed `context`,
    // and vLLM-backed servers reject `max_tokens > max_model_len` outright.
    let max = opts
        .max_tokens
        .unwrap_or(model.max_tokens)
        .min(model.context_window);
    body["max_tokens"] = json!(max);

    if let Some(temperature) = opts.temperature {
        body["temperature"] = json!(temperature);
    }

    // Tool definitions: sent as the native OpenAI `tools` array so the model
    // emits `tool_calls` instead of text-based fences.
    if !ctx.tools.is_empty() {
        body["tools"] = Value::Array(
            ctx.tools.iter().map(openai_tool_schema).collect(),
        );
    }

    body
}

fn openai_tool_schema(tool: &ToolDefinition) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": tool.input_schema,
        },
    })
}

fn convert_messages(messages: &[Message], system_prompt: Option<&str>) -> Vec<Value> {
    let mut out = Vec::new();

    if let Some(prompt) = system_prompt {
        out.push(json!({
            "role": "system",
            "content": prompt,
        }));
    }

    for message in messages {
        match message {
            Message::User(user) => out.push(json!({
                "role": "user",
                "content": user.content,
            })),
            Message::Assistant(assistant) => {
                let mut content = String::new();
                let mut tool_calls = Vec::new();
                for block in &assistant.content {
                    match block {
                        AssistantContent::Text { text } if !text.is_empty() => {
                            if !content.is_empty() {
                                content.push('\n');
                            }
                            content.push_str(text);
                        }
                        AssistantContent::Thinking { .. } => {}
                        AssistantContent::ToolCall(call) => {
                            tool_calls.push(json!({
                                "id": call.id,
                                "type": "function",
                                "function": {
                                    "name": call.name,
                                    "arguments": serde_json::to_string(&call.arguments)
                                        .unwrap_or_default(),
                                },
                            }));
                        }
                        // Empty text or any other variant: skip.
                        _ => {}
                    }
                }

                let mut msg = json!({ "role": "assistant" });
                if !content.is_empty() {
                    msg["content"] = json!(content);
                }
                if !tool_calls.is_empty() {
                    msg["tool_calls"] = json!(tool_calls);
                }
                // Skip empty assistant messages — some providers reject them.
                if content.is_empty() && tool_calls.is_empty() {
                    continue;
                }
                out.push(msg);
            }
            Message::ToolResult(result) => out.push(json!({
                "role": "tool",
                "tool_call_id": result.tool_use_id,
                "content": result.content,
            })),
        }
    }

    out
}

struct ToolBuffer {
    id: String,
    name: String,
    arguments: String,
}

struct Parser {
    acc: Accumulator,
    /// Tool call index (from delta.tool_calls[].index) -> buffer.
    tools: HashMap<u64, ToolBuffer>,
    stop_reason: StopReason,
    errored: bool,
    tool_use_seen: bool,
}

impl Parser {
    fn new(model: &Model) -> Self {
        Self {
            acc: Accumulator::new(model),
            tools: HashMap::new(),
            stop_reason: StopReason::Stop,
            errored: false,
            tool_use_seen: false,
        }
    }

    fn finish(mut self, model: &Model, sink: &EventSink) {
        if self.errored {
            return;
        }
        // Emit ToolCallEnd for each buffered tool call before the final message.
        // OpenAI Completions doesn't have a per-call done event, so we finalize
        // all buffered calls here.
        let mut sorted_tools: Vec<(u64, ToolBuffer)> = self.tools.into_iter().collect();
        sorted_tools.sort_by_key(|(idx, _)| *idx);
        for (idx, tool) in sorted_tools {
            let arguments = serde_json::from_str(&tool.arguments)
                .unwrap_or(Value::Object(Default::default()));
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
            if let Some(thinking) = delta.get(field).and_then(|v| v.as_str()).filter(|t| !t.is_empty()) {
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
                let entry = self.tools.entry(idx).or_insert_with(|| {
                    ToolBuffer {
                        id: String::new(),
                        name: String::new(),
                        arguments: String::new(),
                    }
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
                if let Some(args) = tc["function"]["arguments"].as_str() {
                    if !args.is_empty() {
                        sink.push(AssistantEvent::ToolCallDelta {
                            index: ours,
                            delta: args.to_owned(),
                        });
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{build_body, convert_messages};
    use crate::types::{
        Api, AssistantContent, AssistantMessage, Auth, Context, CostRates, Message, Model,
        StreamOptions, UserMessage,
    };

    fn model() -> Model {
        Model {
            id: "gpt-test".to_owned(),
            name: "Test".to_owned(),
            api: Api::OpenAiCompletions,
            provider: "test".to_owned(),
            base_url: "https://api.test.com/v1".to_owned(),
            reasoning: false,
            input: vec![],
            cost: CostRates::default(),
            context_window: 8192,
            max_tokens: 1024,
            headers: BTreeMap::new(),
            thinking_levels: None,
        }
    }

    #[test]
    fn build_body_emits_chat_completions_shape() {
        let ctx = Context {
            system_prompt: Some("be helpful".to_owned()),
            messages: vec![Message::User(UserMessage::new("hi"))],
            ..Default::default()
        };
        let opts = StreamOptions::new(Auth::ApiKey("k".to_owned()));
        let body = build_body(&model(), &ctx, &opts);

        assert_eq!(body["model"], "gpt-test");
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert_eq!(body["max_tokens"], 1024);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "be helpful");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["messages"][1]["content"], "hi");
    }

    #[test]
    fn build_body_clamps_max_tokens_to_context_window() {
        // Mirror the neuralwatt/glm-5.2 misreport: a model whose advertised
        // `max_tokens` exceeds its `context_window`. The body's request
        // `max_tokens` (which is "max output tokens") must never exceed the
        // context window; vLLM-backed servers reject `max_tokens >
        // max_model_len` outright.
        let mut m = model();
        m.context_window = 8192;
        m.max_tokens = 1_000_000;
        let ctx = Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage::new("hi"))],
            ..Default::default()
        };
        let opts = StreamOptions::new(Auth::ApiKey("k".to_owned()));
        let body = build_body(&m, &ctx, &opts);
        assert_eq!(body["max_tokens"], 8192);
    }

    #[test]
    fn build_body_respects_explicit_max_tokens_options() {
        // A user-supplied opts.max_tokens also must be clamped.
        let mut m = model();
        m.context_window = 8192;
        m.max_tokens = 1_000_000;
        let ctx = Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage::new("hi"))],
            ..Default::default()
        };
        let mut opts = StreamOptions::new(Auth::ApiKey("k".to_owned()));
        opts.max_tokens = Some(20_000);
        let body = build_body(&m, &ctx, &opts);
        assert_eq!(body["max_tokens"], 8192);
    }

    #[test]
    fn convert_messages_skips_empty_assistant() {
        let assistant = AssistantMessage::empty(&model());
        let messages = vec![
            Message::User(UserMessage::new("first")),
            Message::Assistant(assistant),
            Message::User(UserMessage::new("second")),
        ];
        let out = convert_messages(&messages, None);
        // Empty assistant (no text, no tool calls) is dropped.
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["role"], "user");
        assert_eq!(out[1]["role"], "user");
    }

    #[test]
    fn convert_messages_includes_tool_calls() {
        let mut assistant = AssistantMessage::empty(&model());
        assistant.content.push(AssistantContent::Text {
            text: "calling".to_owned(),
        });
        assistant.content.push(AssistantContent::ToolCall(crate::types::ToolCall {
            id: "call_1".to_owned(),
            name: "get_weather".to_owned(),
            arguments: serde_json::json!({"city": "SF"}),
        }));
        let messages = vec![Message::Assistant(assistant)];
        let out = convert_messages(&messages, None);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["role"], "assistant");
        assert_eq!(out[0]["content"], "calling");
        let tool_calls = out[0]["tool_calls"].as_array().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0]["id"], "call_1");
        assert_eq!(tool_calls[0]["function"]["name"], "get_weather");
        assert_eq!(
            tool_calls[0]["function"]["arguments"].as_str(),
            Some(r#"{"city":"SF"}"#)
        );
    }

    #[test]
    fn convert_messages_joins_multiple_text_blocks() {
        let mut assistant = AssistantMessage::empty(&model());
        assistant.content.push(AssistantContent::Text {
            text: "line1".to_owned(),
        });
        assistant.content.push(AssistantContent::Text {
            text: "line2".to_owned(),
        });
        let messages = vec![Message::Assistant(assistant)];
        let out = convert_messages(&messages, None);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["content"], "line1
line2");
    }

    #[test]
    fn build_url_targets_chat_completions() {
        let m = model();
        let url = super::endpoint_url(&m);
        assert_eq!(url, "https://api.test.com/v1/chat/completions");
    }
}