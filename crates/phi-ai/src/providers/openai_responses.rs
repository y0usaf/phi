//! OpenAI Responses API provider (SSE streaming).

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::providers::{SseHandler, http_client, run_sse};
use crate::registry::ApiProvider;
use crate::stream::{Accumulator, AssistantEvent, AssistantEventStream, EventSink};
use crate::types::{
    Api, AssistantContent, AssistantMessage, Auth, Context, Message, Model, StopReason,
    StreamOptions,
};

const CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";

pub struct OpenAiResponsesProvider {
    client: reqwest::Client,
}

impl OpenAiResponsesProvider {
    pub fn new() -> Self {
        Self {
            client: http_client(),
        }
    }
}

impl Default for OpenAiResponsesProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl ApiProvider for OpenAiResponsesProvider {
    fn api(&self) -> Api {
        Api::OpenAiResponses
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
    let base_url = match &opts.auth {
        Auth::ChatGptOAuth { .. } => CODEX_BASE_URL,
        _ => model.base_url.trim_end_matches('/'),
    };
    let url = format!("{base_url}/responses");
    let mut handler = Parser::new(&model);

    let build_request = |client: &reqwest::Client| {
        let mut request = client
            .post(&url)
            .bearer_auth(opts.auth.secret())
            .header("accept", "text/event-stream");
        if let Auth::ChatGptOAuth { account_id, .. } = &opts.auth {
            request = request
                .header("chatgpt-account-id", account_id)
                .header("originator", "phi");
        }
        for (name, value) in &model.headers {
            request = request.header(name, value);
        }
        request.json(&body)
    };

    if run_sse(&client, build_request, &model, &opts, &sink, &mut handler).await {
        handler.finish(&model, &sink);
    }
}

fn build_body(model: &Model, ctx: &Context, opts: &StreamOptions) -> Value {
    let chatgpt = matches!(opts.auth, Auth::ChatGptOAuth { .. });
    let mut body = json!({
        "model": model.id,
        "stream": true,
        "store": false,
        "input": convert_messages(&ctx.messages),
    });

    if chatgpt {
        // The Codex backend requires instructions and rejects max_output_tokens.
        body["instructions"] = json!(
            ctx.system_prompt
                .as_deref()
                .unwrap_or("You are a helpful assistant.")
        );
        body["include"] = json!(["reasoning.encrypted_content"]);
    } else {
        body["max_output_tokens"] = json!(opts.max_tokens.unwrap_or(model.max_tokens));
        if let Some(prompt) = &ctx.system_prompt {
            body["instructions"] = json!(prompt);
        }
    }

    if let Some(effort) = model.thinking_value(opts.thinking) {
        body["reasoning"] = json!({ "effort": effort, "summary": "auto" });
    } else if let Some(temperature) = opts.temperature {
        body["temperature"] = json!(temperature);
    }

    body
}

fn convert_messages(messages: &[Message]) -> Vec<Value> {
    let mut out = Vec::new();
    for message in messages {
        match message {
            Message::User(user) => out.push(json!({
                "role": "user",
                "content": [{ "type": "input_text", "text": user.content }],
            })),
            Message::Assistant(assistant) => {
                let mut content = Vec::new();
                let text = assistant.text();
                if !text.is_empty() {
                    content.push(json!({ "type": "output_text", "text": text }));
                }
                for block in &assistant.content {
                    if let AssistantContent::ToolCall(call) = block {
                        content.push(json!({
                            "type": "function_call",
                            "call_id": call.id,
                            "name": call.name,
                            "arguments": call.arguments,
                        }));
                    }
                }
                if !content.is_empty() {
                    out.push(json!({ "role": "assistant", "content": content }));
                }
            }
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
    /// Responses item id -> our content index.
    items: HashMap<String, usize>,
    /// Responses item id -> tool call buffer (function_call items).
    tools: HashMap<String, ToolBuffer>,
    stop_reason: StopReason,
    usage_seen: bool,
    errored: bool,
    tool_use_seen: bool,
}

impl Parser {
    fn new(model: &Model) -> Self {
        Self {
            acc: Accumulator::new(model),
            items: HashMap::new(),
            tools: HashMap::new(),
            stop_reason: StopReason::Stop,
            usage_seen: false,
            errored: false,
            tool_use_seen: false,
        }
    }

    fn finish(mut self, model: &Model, sink: &EventSink) {
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
                    self.tools.insert(
                        item_id.clone(),
                        ToolBuffer {
                            id: item_id,
                            name,
                            arguments,
                        },
                    );
                    self.tool_use_seen = true;
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
                    && let Some(tool) = self.tools.get_mut(&item_id) {
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
                        let arguments = serde_json::from_str(&tool.arguments)
                            .unwrap_or(Value::Object(Default::default()));
                        self.acc.push_tool_call(crate::types::ToolCall {
                            id: tool.id,
                            name: tool.name,
                            arguments,
                        });
                    }
                }
            }
            _ => {}
        }
    }
}
