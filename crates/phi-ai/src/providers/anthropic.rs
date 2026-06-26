//! Anthropic Messages API provider (SSE streaming).

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::providers::{SseHandler, http_client, run_sse};
use crate::registry::ApiProvider;
use crate::stream::{Accumulator, AssistantEvent, AssistantEventStream, EventSink};
use crate::types::{
    Api, AssistantContent, AssistantMessage, Auth, Context, Message, Model, StopReason,
    StreamOptions, ToolDefinition,
};

const ANTHROPIC_VERSION: &str = "2023-06-01";
const OAUTH_BETA: &str = "claude-code-20250219,oauth-2025-04-20";
const CLAUDE_CODE_IDENTITY: &str = "You are Claude Code, Anthropic's official CLI for Claude.";
const CLAUDE_CLI_VERSION: &str = "2.0.14";

pub struct AnthropicProvider {
    client: reqwest::Client,
}

impl AnthropicProvider {
    pub fn new() -> Self {
        Self {
            client: http_client(),
        }
    }
}

impl Default for AnthropicProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl ApiProvider for AnthropicProvider {
    fn api(&self) -> Api {
        Api::AnthropicMessages
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
    let url = format!("{}/v1/messages", model.base_url.trim_end_matches('/'));
    let mut handler = Parser::new(&model);

    let build_request = |client: &reqwest::Client| {
        let mut request = client
            .post(&url)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("accept", "application/json");
        request = match &opts.auth {
            Auth::OAuthToken(token) => request
                .bearer_auth(token)
                .header("anthropic-beta", OAUTH_BETA)
                .header("user-agent", format!("claude-cli/{CLAUDE_CLI_VERSION}"))
                .header("x-app", "cli"),
            Auth::ApiKey(key) => request.header("x-api-key", key),
            Auth::ChatGptOAuth { access, .. } => request.bearer_auth(access),
        };
        for (name, value) in &model.headers {
            request = request.header(name, value);
        }
        request.json(&body)
    };

    if run_sse(&client, build_request, &model, &opts, &sink, &mut handler).await {
        handler.finish(&model, &sink);
    }
}

fn anthropic_tool_schema(tool: &ToolDefinition) -> Value {
    json!({
        "name": tool.name,
        "description": tool.description,
        "input_schema": tool.input_schema,
    })
}

fn build_body(model: &Model, ctx: &Context, opts: &StreamOptions) -> Value {
    let is_oauth = matches!(opts.auth, Auth::OAuthToken(_));
    // Cap to context_window defensively (see openai_completions::build_body).
    let max = opts
        .max_tokens
        .unwrap_or(model.max_tokens)
        .min(model.context_window);
    let mut body = json!({
        "model": model.id,
        "max_tokens": max,
        "stream": true,
        "messages": convert_messages(&ctx.messages),
    });

    let mut system = Vec::new();
    if is_oauth {
        // OAuth requests must carry the Claude Code identity as the first
        // system block or the API rejects the token.
        system.push(json!({ "type": "text", "text": CLAUDE_CODE_IDENTITY }));
    }
    if let Some(prompt) = &ctx.system_prompt {
        system.push(json!({
            "type": "text",
            "text": prompt,
            "cache_control": { "type": "ephemeral" },
        }));
    }
    if !system.is_empty() {
        body["system"] = Value::Array(system);
    }

    // Tool definitions: sent as the native Anthropic `tools` array so the
    // model emits `tool_use` content blocks instead of text-based fences.
    if !ctx.tools.is_empty() {
        body["tools"] = Value::Array(
            ctx.tools.iter().map(anthropic_tool_schema).collect(),
        );
    }

    let thinking_budget = model
        .thinking_value(opts.thinking)
        .and_then(|v| v.parse::<u64>().ok());
    if let Some(budget) = thinking_budget {
        body["thinking"] = json!({ "type": "enabled", "budget_tokens": budget });
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
                "content": [{ "type": "text", "text": user.content }],
            })),
            Message::Assistant(assistant) => {
                let mut content = Vec::new();
                for block in &assistant.content {
                    match block {
                        AssistantContent::Text { text } if !text.is_empty() => {
                            content.push(json!({ "type": "text", "text": text }));
                        }
                        AssistantContent::Thinking {
                            thinking,
                            signature: Some(signature),
                        } => {
                            content.push(json!({
                                "type": "thinking",
                                "thinking": thinking,
                                "signature": signature,
                            }));
                        }
                        AssistantContent::ToolCall(call) => {
                            content.push(json!({
                                "type": "tool_use",
                                "id": call.id,
                                "name": call.name,
                                "input": call.arguments,
                            }));
                        }
                        _ => {}
                    }
                }
                if !content.is_empty() {
                    out.push(json!({ "role": "assistant", "content": content }));
                }
            }
            Message::ToolResult(result) => out.push(json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": result.tool_use_id,
                    "content": result.content,
                    "is_error": result.is_error,
                }],
            })),
        }
    }

    // Place a single cache breakpoint on the last content block of the last
    // *user* message. This matches pi's strategy: only user messages get
    // cache_control, and only the single most-recent user turn. The system
    // prompt already uses one breakpoint (see build_body); Anthropic allows 4
    // total, so we keep one on the conversation for cache-read pricing (10x
    // cheaper than full input) on subsequent turns.
    for msg in out.iter_mut().rev() {
        if msg.get("role").and_then(|r| r.as_str()) == Some("user") {
            if let Some(content) = msg.get_mut("content").and_then(|c| c.as_array_mut())
                && let Some(last_block) = content.last_mut()
            {
                last_block["cache_control"] = json!({ "type": "ephemeral" });
            }
            break;
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
    /// Anthropic content index -> our content index (tool blocks skew them).
    index_map: HashMap<u64, usize>,
    tools: HashMap<u64, ToolBuffer>,
    stop_reason: StopReason,
    errored: bool,
}

impl Parser {
    fn new(model: &Model) -> Self {
        Self {
            acc: Accumulator::new(model),
            index_map: HashMap::new(),
            tools: HashMap::new(),
            stop_reason: StopReason::Stop,
            errored: false,
        }
    }

    fn finish(self, model: &Model, sink: &EventSink) {
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
                    let arguments = serde_json::from_str(&tool.arguments)
                        .unwrap_or_else(|e| {
                            eprintln!("anthropic: malformed tool-call arguments for index {index}: {e}");
                            Value::Object(Default::default())
                        });
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::types::{
        Api, Auth, Context, CostRates, Message, Model, StreamOptions, ToolDefinition,
        ToolResultMessage, UserMessage,
    };

    use super::{CLAUDE_CODE_IDENTITY, build_body, convert_messages};
    use crate::types::{AssistantContent, AssistantMessage};
    use serde_json::json;

    /// Minimal Anthropic model for exercising `build_body`'s system-block
    /// assembly (no reasoning, no thinking levels).
    fn model() -> Model {
        Model {
            id: "claude-test".to_owned(),
            name: "Test".to_owned(),
            api: Api::AnthropicMessages,
            provider: "anthropic".to_owned(),
            base_url: "https://api.anthropic.com".to_owned(),
            reasoning: false,
            input: vec![],
            cost: CostRates::default(),
            context_window: 200_000,
            max_tokens: 4096,
            headers: BTreeMap::new(),
            thinking_levels: None,
        }
    }

    /// The substantive RLM identity that `build_system_prompt` produces and that
    /// `ctx.system_prompt` carries in the real flow.
    const RLM_IDENTITY: &str = "You are a Recursive Language Model (RLM): a language model with a prompt, and a context stored in a Python REPL.";

    /// Track 0 (plans/prompt-content-port.md): the API-key path emits exactly
    /// one system block — the cached RLM identity — with no OAuth "Claude Code"
    /// shim. Ported content must never reinforce the "Claude Code" identity in
    /// the cached block.
    #[test]
    fn api_key_path_emits_single_cached_system_block() {
        let ctx = Context {
            system_prompt: Some(RLM_IDENTITY.to_owned()),
            messages: vec![],
            ..Default::default()
        };
        let opts = StreamOptions::new(Auth::ApiKey("k".to_owned()));
        let body = build_body(&model(), &ctx, &opts);
        let system = body["system"].as_array().expect("system array");
        assert_eq!(system.len(), 1, "api-key path must emit exactly one system block");
        assert_eq!(system[0]["text"].as_str(), Some(RLM_IDENTITY));
        assert!(
            system[0].get("cache_control").is_some(),
            "the RLM block is the cached block"
        );
        assert!(
            !system[0]["text"]
                .as_str()
                .unwrap()
                .contains("Claude Code, Anthropic's official CLI"),
            "cached block must not duplicate the OAuth identity"
        );
    }

    /// Track 0: the OAuth path emits the compliance shim (uncached) followed by
    /// the cached RLM identity, in that order. The shim is a carrier required
    /// to validate the token; the RLM identity is the substantive one.
    #[test]
    fn oauth_path_emits_shim_then_cached_rlm_block() {
        let ctx = Context {
            system_prompt: Some(RLM_IDENTITY.to_owned()),
            messages: vec![],
            ..Default::default()
        };
        let opts = StreamOptions::new(Auth::OAuthToken("token".to_owned()));
        let body = build_body(&model(), &ctx, &opts);
        let system = body["system"].as_array().expect("system array");
        assert_eq!(system.len(), 2, "oauth path: shim + cached RLM block");
        // Block 0: the OAuth compliance shim, uncached.
        assert_eq!(system[0]["text"].as_str(), Some(CLAUDE_CODE_IDENTITY));
        assert!(
            system[0].get("cache_control").is_none(),
            "the OAuth shim is uncached"
        );
        // Block 1: the substantive RLM identity, cached.
        assert_eq!(system[1]["text"].as_str(), Some(RLM_IDENTITY));
        assert!(
            system[1].get("cache_control").is_some(),
            "the RLM block is the cached block"
        );
        assert!(
            system[1]["text"].as_str().unwrap().contains("Recursive Language Model"),
            "the cached block is the RLM identity, not the shim"
        );
    }

    /// `convert_messages` must place `cache_control: ephemeral` on the last
    /// content block of the last *user* message only — matching pi's strategy
    /// of a single breakpoint on the most-recent user turn.
    #[test]
    fn convert_messages_places_cache_breakpoint_on_last_user_message() {
        let messages = vec![
            Message::User(UserMessage::new("hello")),
            Message::User(UserMessage::new("world")),
            Message::User(UserMessage::new("third message")),
        ];

        let result = convert_messages(&messages);
        assert_eq!(result.len(), 3);

        // First two messages: no cache_control.
        for i in [0, 1] {
            let content = result[i]["content"].as_array().unwrap();
            assert!(
                content[0].get("cache_control").is_none(),
                "message {i} must not be cached"
            );
        }

        // Only the last (third) user message: last content block has cache_control.
        let content = result[2]["content"].as_array().unwrap();
        let last_block = content.last().unwrap();
        assert_eq!(
            last_block["cache_control"]["type"].as_str(),
            Some("ephemeral"),
            "last user message must be cached"
        );
    }

    /// With a single message, only that message gets a cache breakpoint.
    #[test]
    fn convert_messages_single_message_gets_one_breakpoint() {
        let messages = vec![Message::User(UserMessage::new("only message"))];
        let result = convert_messages(&messages);
        let content = result[0]["content"].as_array().unwrap();
        assert_eq!(
            content[0]["cache_control"]["type"].as_str(),
            Some("ephemeral")
        );
    }

    /// Empty messages produce no output and no breakpoints.
    #[test]
    fn convert_messages_empty() {
        let result = convert_messages(&[]);
        assert!(result.is_empty());
    }

    /// When the last message is an assistant message (e.g. the conversation
    /// ends with an assistant turn), no cache breakpoint is placed — matching
    /// pi's `lastMessage.role === "user"` guard.
    #[test]
    fn convert_messages_no_breakpoint_when_last_message_is_assistant() {
        let mut assistant = AssistantMessage::empty(&model());
        assistant.content.push(AssistantContent::Text {
            text: "hi there".to_owned(),
        });
        let messages = vec![
            Message::User(UserMessage::new("hello")),
            Message::Assistant(assistant),
        ];

        let result = convert_messages(&messages);
        assert_eq!(result.len(), 2);

        // The last user message (message 0) gets the cache breakpoint.
        let user_content = result[0]["content"].as_array().unwrap();
        assert_eq!(
            user_content[0]["cache_control"]["type"].as_str(),
            Some("ephemeral"),
            "the last user message gets cached even when followed by assistant"
        );

        // Assistant message (second): no cache_control.
        let asst_content = result[1]["content"].as_array().unwrap();
        assert!(
            asst_content[0].get("cache_control").is_none(),
            "assistant messages never get cache_control"
        );
    }

    /// When the conversation ends with [user, assistant, user], only the
    /// final user message gets the breakpoint — not the assistant or the
    /// first user.
    #[test]
    fn convert_messages_only_last_user_gets_breakpoint() {
        let mut assistant = AssistantMessage::empty(&model());
        assistant.content.push(AssistantContent::Text {
            text: "response".to_owned(),
        });
        let messages = vec![
            Message::User(UserMessage::new("first")),
            Message::Assistant(assistant),
            Message::User(UserMessage::new("second")),
        ];

        let result = convert_messages(&messages);
        assert_eq!(result.len(), 3);

        // First user: no cache_control.
        assert!(result[0]["content"].as_array().unwrap()[0]
            .get("cache_control")
            .is_none());

        // Assistant: no cache_control.
        assert!(result[1]["content"].as_array().unwrap()[0]
            .get("cache_control")
            .is_none());

        // Last user: has cache_control.
        assert_eq!(
            result[2]["content"].as_array().unwrap()[0]["cache_control"]["type"]
                .as_str(),
            Some("ephemeral")
        );
    }

    // --- Tool schema tests ---

    #[test]
    fn anthropic_tool_schema_emits_name_description_input_schema() {
        let tool = ToolDefinition {
            name: "repl".to_owned(),
            description: "Execute Python code".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "code": { "type": "string" }
                },
                "required": ["code"]
            }),
        };
        let schema = super::anthropic_tool_schema(&tool);
        assert_eq!(schema["name"], "repl");
        assert_eq!(schema["description"], "Execute Python code");
        assert_eq!(schema["input_schema"]["type"], "object");
        assert_eq!(schema["input_schema"]["properties"]["code"]["type"], "string");
        // Anthropic uses `input_schema` at the root (not nested under `function`).
        assert!(schema.get("function").is_none(), "anthropic schema must not nest under function");
    }

    #[test]
    fn build_body_includes_tools_array_when_tools_present() {
        let ctx = Context {
            system_prompt: Some("sys".to_owned()),
            messages: vec![Message::User(UserMessage::new("hi"))],
            tools: vec![ToolDefinition {
                name: "repl".to_owned(),
                description: "Run code".to_owned(),
                input_schema: json!({"type": "object"}),
            }],
            ..Default::default()
        };
        let opts = StreamOptions::new(Auth::ApiKey("k".to_owned()));
        let body = build_body(&model(), &ctx, &opts);
        let tools = body["tools"].as_array().expect("tools array");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "repl");
        assert_eq!(tools[0]["input_schema"]["type"], "object");
    }

    #[test]
    fn build_body_omits_tools_array_when_empty() {
        let ctx = Context {
            system_prompt: Some("sys".to_owned()),
            messages: vec![Message::User(UserMessage::new("hi"))],
            ..Default::default()
        };
        let opts = StreamOptions::new(Auth::ApiKey("k".to_owned()));
        let body = build_body(&model(), &ctx, &opts);
        assert!(body.get("tools").is_none(), "no tools => no tools key");
    }

    // --- convert_messages with ToolCall and ToolResult ---

    #[test]
    fn convert_messages_serializes_assistant_tool_call() {
        let mut assistant = AssistantMessage::empty(&model());
        assistant.content.push(AssistantContent::Text {
            text: "let me check".to_owned(),
        });
        assistant.content.push(AssistantContent::ToolCall(crate::types::ToolCall {
            id: "toolu_123".to_owned(),
            name: "repl".to_owned(),
            arguments: json!({"code": "print(1+1)"}),
        }));
        let messages = vec![Message::Assistant(assistant)];
        let out = convert_messages(&messages);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["role"], "assistant");
        let content = out[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "tool_use");
        assert_eq!(content[1]["id"], "toolu_123");
        assert_eq!(content[1]["name"], "repl");
        assert_eq!(content[1]["input"]["code"], "print(1+1)");
    }

    #[test]
    fn convert_messages_serializes_tool_result_as_user_tool_result() {
        let result = ToolResultMessage::new("toolu_123", "2");
        let messages = vec![Message::ToolResult(result)];
        let out = convert_messages(&messages);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["role"], "user");
        let content = out[0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "tool_result");
        assert_eq!(content[0]["tool_use_id"], "toolu_123");
        assert_eq!(content[0]["content"], "2");
        assert_eq!(content[0]["is_error"], false);
    }

    #[test]
    fn convert_messages_serializes_tool_result_with_error_flag() {
        let result = ToolResultMessage::new("toolu_456", "Error: bad code").with_error(true);
        let messages = vec![Message::ToolResult(result)];
        let out = convert_messages(&messages);
        let content = out[0]["content"].as_array().unwrap();
        assert_eq!(content[0]["is_error"], true);
    }

    #[test]
    fn convert_messages_round_trips_assistant_tool_call_then_tool_result() {
        let mut assistant = AssistantMessage::empty(&model());
        assistant.content.push(AssistantContent::ToolCall(crate::types::ToolCall {
            id: "toolu_789".to_owned(),
            name: "repl".to_owned(),
            arguments: json!({"code": "x = 1"}),
        }));
        let result = ToolResultMessage::new("toolu_789", "ok");
        let messages = vec![
            Message::User(UserMessage::new("what is 1+1?")),
            Message::Assistant(assistant),
            Message::ToolResult(result),
            Message::User(UserMessage::new("thanks")),
        ];
        let out = convert_messages(&messages);
        assert_eq!(out.len(), 4);
        let asst_content = out[1]["content"].as_array().unwrap();
        assert_eq!(asst_content[0]["type"], "tool_use");
        assert_eq!(out[2]["role"], "user");
        assert_eq!(out[2]["content"][0]["type"], "tool_result");
        assert_eq!(out[2]["content"][0]["tool_use_id"], "toolu_789");
        let last_user_content = out[3]["content"].as_array().unwrap();
        assert_eq!(
            last_user_content[0]["cache_control"]["type"].as_str(),
            Some("ephemeral")
        );
    }
}
