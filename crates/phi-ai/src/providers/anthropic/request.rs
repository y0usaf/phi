use serde_json::{Value, json};

use crate::types::{
    AssistantContent, Auth, Context, Message, Model, StreamOptions, ToolDefinition,
};

pub(super) const ANTHROPIC_VERSION: &str = "2023-06-01";
pub(super) const OAUTH_BETA: &str = "claude-code-20250219,oauth-2025-04-20";
/// Compliance shim the API requires as the first system block to honor OAuth
/// tokens; without it every request is rejected with a 429 rate_limit_error.
pub(super) const CLAUDE_CODE_IDENTITY: &str =
    "You are Claude Code, Anthropic's official CLI for Claude.";
pub(super) const CLAUDE_CLI_VERSION: &str = "2.0.14";
pub(super) const INTERLEAVED_THINKING_BETA: &str = "interleaved-thinking-2025-05-14";

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
    if is_oauth {
        // OAuth requests require a carrier identity block as the first
        // system block; `opts.identity` may override the default shim.
        let identity = opts.identity.as_deref().unwrap_or(CLAUDE_CODE_IDENTITY);
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
    // When thinking is enabled we leave `tool_choice` at the API default
    // (`auto`) so thinking and tool use coexist — matching pi's behavior
    // (pi's `streamSimpleAnthropic` never sets `tool_choice`). When thinking
    // is disabled we force `tool_choice: {"type": "any"}` so the model must
    // call a tool every turn instead of looping in prose.
    let has_tools = !ctx.tools.is_empty();
    if has_tools {
        body["tools"] = Value::Array(ctx.tools.iter().map(anthropic_tool_schema).collect());
    }

    // Compute the thinking value (effort string for adaptive models, token
    // budget for budget-based models). `thinking_value` returns None when
    // thinking is off or unsupported.
    let thinking_val = model.thinking_value(opts.thinking);
    let thinking_enabled = thinking_val.is_some();

    if has_tools && !thinking_enabled {
        // No thinking: force a tool call to prevent prose looping.
        body["tool_choice"] = json!({ "type": "any" });
    }

    if let Some(val) = thinking_val {
        if model.adaptive_thinking {
            // Adaptive thinking: the model decides when/how much to think.
            // The effort string controls reasoning depth via `output_config`.
            body["thinking"] = json!({ "type": "adaptive", "display": "summarized" });
            body["output_config"] = json!({ "effort": val });
        } else {
            // Budget-based thinking for older Anthropic models.
            let budget = val.parse::<u64>().unwrap_or(1024);
            body["thinking"] = json!({
                "type": "enabled",
                "budget_tokens": budget,
                "display": "summarized",
            });
        }
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
                        // Unsigned thinking blocks (e.g. from the Vercel AI
                        // Gateway for non-Anthropic reasoning models like GLM)
                        // cannot be replayed as `type: "thinking"` because real
                        // Anthropic rejects empty signatures. Convert to plain
                        // text so the reasoning context is preserved across
                        // turns. This matches pi's default behavior (without
                        // `allowEmptySignature`).
                        AssistantContent::Thinking {
                            thinking,
                            signature: None,
                        } if !thinking.is_empty() => {
                            content.push(json!({
                                "type": "text",
                                "text": thinking,
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
            adaptive_thinking: false,
            reasoning_control: None,
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
    /// to validate the token; the RLM identity is the substantive one. The
    /// shim is applied by default — no `opts.identity` needed.
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
            system[1]["text"]
                .as_str()
                .unwrap()
                .contains("Recursive Language Model"),
            "the cached block is the RLM identity, not the shim"
        );
    }

    /// A caller-supplied `opts.identity` overrides the default shim.
    #[test]
    fn oauth_path_custom_identity_overrides_default_shim() {
        let ctx = Context {
            system_prompt: Some(RLM_IDENTITY.to_owned()),
            messages: vec![],
            ..Default::default()
        };
        let mut opts = StreamOptions::new(Auth::OAuthToken("token".to_owned()));
        opts.identity = Some("custom identity".to_owned());
        let body = build_body(&model(), &ctx, &opts);
        let system = body["system"].as_array().expect("system array");
        assert_eq!(system.len(), 2, "custom identity => shim + system prompt");
        assert_eq!(system[0]["text"].as_str(), Some("custom identity"));
        assert_eq!(system[1]["text"].as_str(), Some(RLM_IDENTITY));
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

    /// When thinking is enabled and tools are present, thinking is preserved
    /// and `tool_choice` is NOT forced — matching pi's behavior (pi never sets
    /// `tool_choice` in its simple stream path). Thinking + tools coexist via
    /// the interleaved-thinking beta header (added in `mod.rs`).
    #[test]
    fn build_body_preserves_thinking_with_tools() {
        // A budget-based (non-adaptive) reasoning model with thinking levels.
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
        // tool_choice is NOT forced when thinking is enabled
        assert!(
            body.get("tool_choice").is_none(),
            "tool_choice must not be forced when thinking is enabled"
        );
        // thinking IS present (coexists with tools)
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["thinking"]["budget_tokens"], 8000);
        assert_eq!(body["thinking"]["display"], "summarized");
    }

    /// When thinking is disabled and tools are present, `tool_choice` is
    /// forced to `any` to prevent the model from looping in prose.
    #[test]
    fn build_body_forces_tool_choice_when_thinking_off_and_tools_present() {
        let m = model(); // reasoning=false => no thinking

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
        let body = build_body(&m, &ctx, &opts);
        assert_eq!(body["tool_choice"]["type"], "any");
        assert!(body.get("thinking").is_none());
    }

    /// Adaptive thinking models send `thinking.type: "adaptive"` and
    /// `output_config.effort` instead of budget-based thinking.
    #[test]
    fn build_body_adaptive_thinking_sends_effort() {
        let mut m = model();
        m.reasoning = true;
        m.adaptive_thinking = true;
        let mut levels = BTreeMap::new();
        levels.insert(ThinkingLevel::High, Some("high".to_owned()));
        levels.insert(ThinkingLevel::XHigh, Some("xhigh".to_owned()));
        m.thinking_levels = Some(levels);

        let ctx = Context {
            system_prompt: Some("sys".to_owned()),
            messages: vec![Message::User(UserMessage::new("hi"))],
            ..Default::default()
        };
        let mut opts = StreamOptions::new(Auth::ApiKey("k".to_owned()));
        opts.thinking = ThinkingLevel::XHigh;
        let body = build_body(&m, &ctx, &opts);
        assert_eq!(body["thinking"]["type"], "adaptive");
        assert_eq!(body["thinking"]["display"], "summarized");
        assert_eq!(body["output_config"]["effort"], "xhigh");
        // No budget_tokens for adaptive models
        assert!(body["thinking"].get("budget_tokens").is_none());
    }

    /// Adaptive thinking + tools: thinking preserved, no forced tool_choice.
    #[test]
    fn build_body_adaptive_thinking_with_tools() {
        let mut m = model();
        m.reasoning = true;
        m.adaptive_thinking = true;
        let mut levels = BTreeMap::new();
        levels.insert(ThinkingLevel::High, Some("high".to_owned()));
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
        opts.thinking = ThinkingLevel::High;
        let body = build_body(&m, &ctx, &opts);
        assert_eq!(body["thinking"]["type"], "adaptive");
        assert_eq!(body["output_config"]["effort"], "high");
        assert!(
            body.get("tool_choice").is_none(),
            "no forced tool_choice for adaptive thinking + tools"
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
        assert_eq!(body["thinking"]["display"], "summarized");
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

    /// Unsigned thinking blocks (signature: None, e.g. from the Vercel AI
    /// Gateway for non-Anthropic reasoning models like GLM) should be
    /// converted to plain text on replay — not dropped silently. This
    /// matches pi's default behavior (without `allowEmptySignature`).
    #[test]
    fn convert_messages_unsigned_thinking_converted_to_text() {
        let mut assistant = AssistantMessage::empty(&model());
        assistant.content.push(AssistantContent::Thinking {
            thinking: "Let me analyze this...".to_owned(),
            signature: None,
        });
        assistant.content.push(AssistantContent::Text {
            text: "Here is the answer.".to_owned(),
        });
        let messages = vec![
            Message::User(UserMessage::new("hi")),
            Message::Assistant(assistant),
            Message::User(UserMessage::new("thanks")),
        ];
        let out = convert_messages(&messages, false);
        let asst_content = out[1]["content"].as_array().unwrap();
        // Unsigned thinking becomes a text block (not a thinking block).
        assert_eq!(asst_content[0]["type"], "text");
        assert_eq!(asst_content[0]["text"], "Let me analyze this...");
        // The actual response text is preserved as a separate block.
        assert_eq!(asst_content[1]["type"], "text");
        assert_eq!(asst_content[1]["text"], "Here is the answer.");
    }

    /// Signed thinking blocks should be replayed as `type: "thinking"` with
    /// the signature intact (this is the existing behavior, verified here to
    /// ensure the unsigned fix didn't break it).
    #[test]
    fn convert_messages_signed_thinking_preserved() {
        let mut assistant = AssistantMessage::empty(&model());
        assistant.content.push(AssistantContent::Thinking {
            thinking: "Reasoning here...".to_owned(),
            signature: Some("sig_abc123".to_owned()),
        });
        let messages = vec![
            Message::User(UserMessage::new("hi")),
            Message::Assistant(assistant),
            Message::User(UserMessage::new("thanks")),
        ];
        let out = convert_messages(&messages, false);
        let asst_content = out[1]["content"].as_array().unwrap();
        assert_eq!(asst_content[0]["type"], "thinking");
        assert_eq!(asst_content[0]["thinking"], "Reasoning here...");
        assert_eq!(asst_content[0]["signature"], "sig_abc123");
    }
}
