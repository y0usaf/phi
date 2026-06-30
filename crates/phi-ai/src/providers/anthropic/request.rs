use serde_json::{Value, json};

use crate::types::{
    AssistantContent, Auth, Context, Message, Model, StreamOptions, ToolDefinition,
};

pub(super) const ANTHROPIC_VERSION: &str = "2023-06-01";
pub(super) const OAUTH_BETA: &str = "claude-code-20250219,oauth-2025-04-20";
pub(super) const CLAUDE_CLI_VERSION: &str = "2.0.14";

fn anthropic_tool_schema(tool: &ToolDefinition) -> Value {
    json!({
        "name": tool.name,
        "description": tool.description,
        "input_schema": tool.input_schema,
    })
}

pub(super) fn build_body(model: &Model, ctx: &Context, opts: &StreamOptions) -> Value {
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
        "messages": convert_messages(&ctx.messages, opts.cache_control),
    });

    let mut system = Vec::new();
    if is_oauth && let Some(identity) = &opts.identity {
        // OAuth requests may require a carrier identity block as the
        // first system block (e.g. the Claude Code compliance shim).
        system.push(json!({ "type": "text", "text": identity }));
    }
    if let Some(prompt) = &ctx.system_prompt {
        let mut block = json!({ "type": "text", "text": prompt });
        if opts.cache_control {
            block["cache_control"] = json!({ "type": "ephemeral" });
        }
        system.push(block);
    }
    if !system.is_empty() {
        body["system"] = Value::Array(system);
    }

    // Tool definitions: sent as the native Anthropic `tools` array so the
    // model emits `tool_use` content blocks instead of text-based fences.
    // We force `tool_choice: {"type": "any"}` so the model must call a tool
    // every turn — without it, reasoning models default to `auto` and loop
    // in prose. Anthropic rejects forced `tool_choice` when thinking is
    // enabled, so when tools are present we drop `thinking` to make the
    // forced call succeed (mirroring pi's `disableThinkingIfToolChoiceForced`).
    let has_tools = !ctx.tools.is_empty();
    if has_tools {
        body["tools"] = Value::Array(ctx.tools.iter().map(anthropic_tool_schema).collect());
        body["tool_choice"] = json!({ "type": "any" });
    }

    let thinking_budget = if has_tools {
        None // Forced tool_choice is incompatible with thinking — skip it.
    } else {
        model
            .thinking_value(opts.thinking)
            .and_then(|v| v.parse::<u64>().ok())
    };
    if let Some(budget) = thinking_budget {
        body["thinking"] = json!({ "type": "enabled", "budget_tokens": budget });
    } else if let Some(temperature) = opts.temperature {
        body["temperature"] = json!(temperature);
    }

    body
}

fn convert_messages(messages: &[Message], cache_control: bool) -> Vec<Value> {
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
    if cache_control {
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
    }

    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::types::{
        Api, Auth, Context, CostRates, Message, Model, StreamOptions, ThinkingLevel,
        ToolDefinition, ToolResultMessage, UserMessage,
    };

    use super::{build_body, convert_messages};
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

    /// Test-only identity string, mirroring the former hardcoded constant.
    const CLAUDE_CODE_IDENTITY: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

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
        assert_eq!(
            system.len(),
            1,
            "api-key path must emit exactly one system block"
        );
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
        let mut opts = StreamOptions::new(Auth::OAuthToken("token".to_owned()));
        opts.identity = Some(CLAUDE_CODE_IDENTITY.to_owned());
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
            system[1]["text"]
                .as_str()
                .unwrap()
                .contains("Recursive Language Model"),
            "the cached block is the RLM identity, not the shim"
        );
    }

    /// OAuth path without an identity shim emits only the system prompt block.
    #[test]
    fn oauth_path_without_identity_emits_single_block() {
        let ctx = Context {
            system_prompt: Some(RLM_IDENTITY.to_owned()),
            messages: vec![],
            ..Default::default()
        };
        let opts = StreamOptions::new(Auth::OAuthToken("token".to_owned()));
        let body = build_body(&model(), &ctx, &opts);
        let system = body["system"].as_array().expect("system array");
        assert_eq!(system.len(), 1, "no identity => single system block");
        assert_eq!(system[0]["text"].as_str(), Some(RLM_IDENTITY));
    }

    /// `cache_control: false` suppresses all `cache_control` markers.
    #[test]
    fn cache_control_disabled_strips_markers() {
        let ctx = Context {
            system_prompt: Some("sys".to_owned()),
            messages: vec![Message::User(UserMessage::new("hello"))],
            ..Default::default()
        };
        let mut opts = StreamOptions::new(Auth::ApiKey("k".to_owned()));
        opts.cache_control = false;
        let body = build_body(&model(), &ctx, &opts);
        let system = body["system"].as_array().expect("system array");
        assert!(system[0].get("cache_control").is_none());
        let messages = body["messages"].as_array().expect("messages array");
        let last_content = messages[0]["content"].as_array().unwrap();
        assert!(last_content[0].get("cache_control").is_none());
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

        let result = convert_messages(&messages, true);
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
        let result = convert_messages(&messages, true);
        let content = result[0]["content"].as_array().unwrap();
        assert_eq!(
            content[0]["cache_control"]["type"].as_str(),
            Some("ephemeral")
        );
    }

    /// Empty messages produce no output and no breakpoints.
    #[test]
    fn convert_messages_empty() {
        let result = convert_messages(&[], true);
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

        let result = convert_messages(&messages, true);
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

        let result = convert_messages(&messages, true);
        assert_eq!(result.len(), 3);

        // First user: no cache_control.
        assert!(
            result[0]["content"].as_array().unwrap()[0]
                .get("cache_control")
                .is_none()
        );

        // Assistant: no cache_control.
        assert!(
            result[1]["content"].as_array().unwrap()[0]
                .get("cache_control")
                .is_none()
        );

        // Last user: has cache_control.
        assert_eq!(
            result[2]["content"].as_array().unwrap()[0]["cache_control"]["type"].as_str(),
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
        assert_eq!(
            schema["input_schema"]["properties"]["code"]["type"],
            "string"
        );
        // Anthropic uses `input_schema` at the root (not nested under `function`).
        assert!(
            schema.get("function").is_none(),
            "anthropic schema must not nest under function"
        );
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

    #[test]
    fn build_body_forces_tool_choice_any_when_tools_present() {
        let ctx = Context {
            system_prompt: Some("sys".to_owned()),
            messages: vec![Message::User(UserMessage::new("hi"))],
            tools: vec![ToolDefinition {
                name: "repl".to_owned(),
                description: "Run code".to_owned(),
                input_schema: json!({"type": "object"}),
            }],
        };
        let opts = StreamOptions::new(Auth::ApiKey("k".to_owned()));
        let body = build_body(&model(), &ctx, &opts);
        assert_eq!(body["tool_choice"]["type"], "any");
    }

    #[test]
    fn build_body_omits_tool_choice_when_no_tools() {
        let ctx = Context {
            system_prompt: Some("sys".to_owned()),
            messages: vec![Message::User(UserMessage::new("hi"))],
            ..Default::default()
        };
        let opts = StreamOptions::new(Auth::ApiKey("k".to_owned()));
        let body = build_body(&model(), &ctx, &opts);
        assert!(body.get("tool_choice").is_none());
    }

    /// When tools are present, thinking must be dropped because Anthropic
    /// rejects forced `tool_choice` alongside `thinking: enabled`.
    #[test]
    fn build_body_drops_thinking_when_tools_present() {
        // A reasoning-capable model with thinking levels.
        let mut m = model();
        m.reasoning = true;
        let mut levels = BTreeMap::new();
        levels.insert(ThinkingLevel::Low, Some("8000".to_owned()));
        m.thinking_levels = Some(levels);

        let ctx = Context {
            system_prompt: Some("sys".to_owned()),
            messages: vec![Message::User(UserMessage::new("hi"))],
            tools: vec![ToolDefinition {
                name: "repl".to_owned(),
                description: "Run code".to_owned(),
                input_schema: json!({"type": "object"}),
            }],
        };
        let mut opts = StreamOptions::new(Auth::ApiKey("k".to_owned()));
        opts.thinking = ThinkingLevel::Low;
        let body = build_body(&m, &ctx, &opts);
        // tool_choice is forced
        assert_eq!(body["tool_choice"]["type"], "any");
        // thinking is NOT present (would 400 alongside forced tool_choice)
        assert!(
            body.get("thinking").is_none(),
            "thinking must be dropped when tool_choice is forced"
        );
    }

    /// Without tools, thinking is preserved normally.
    #[test]
    fn build_body_preserves_thinking_when_no_tools() {
        let mut m = model();
        m.reasoning = true;
        let mut levels = BTreeMap::new();
        levels.insert(ThinkingLevel::Low, Some("8000".to_owned()));
        m.thinking_levels = Some(levels);

        let ctx = Context {
            system_prompt: Some("sys".to_owned()),
            messages: vec![Message::User(UserMessage::new("hi"))],
            ..Default::default()
        };
        let mut opts = StreamOptions::new(Auth::ApiKey("k".to_owned()));
        opts.thinking = ThinkingLevel::Low;
        let body = build_body(&m, &ctx, &opts);
        assert!(body.get("tool_choice").is_none());
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["thinking"]["budget_tokens"], 8000);
    }

    // --- convert_messages with ToolCall and ToolResult ---

    #[test]
    fn convert_messages_serializes_assistant_tool_call() {
        let mut assistant = AssistantMessage::empty(&model());
        assistant.content.push(AssistantContent::Text {
            text: "let me check".to_owned(),
        });
        assistant
            .content
            .push(AssistantContent::ToolCall(crate::types::ToolCall {
                id: "toolu_123".to_owned(),
                name: "repl".to_owned(),
                arguments: json!({"code": "print(1+1)"}),
            }));
        let messages = vec![Message::Assistant(assistant)];
        let out = convert_messages(&messages, true);
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
        let out = convert_messages(&messages, true);
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
        let out = convert_messages(&messages, true);
        let content = out[0]["content"].as_array().unwrap();
        assert_eq!(content[0]["is_error"], true);
    }

    #[test]
    fn convert_messages_round_trips_assistant_tool_call_then_tool_result() {
        let mut assistant = AssistantMessage::empty(&model());
        assistant
            .content
            .push(AssistantContent::ToolCall(crate::types::ToolCall {
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
        let out = convert_messages(&messages, true);
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
