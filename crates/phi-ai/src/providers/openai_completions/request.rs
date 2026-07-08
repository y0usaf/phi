use serde_json::{Value, json};

use crate::types::{AssistantContent, Message, Model, ToolDefinition};

pub(super) fn endpoint_url(model: &Model) -> String {
    format!("{}/chat/completions", model.base_url.trim_end_matches('/'))
}

pub(super) fn build_body(
    model: &Model,
    ctx: &crate::types::Context,
    opts: &crate::types::StreamOptions,
) -> Value {
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

    // Reasoning control: write the value the model declared for the
    // requested `ThinkingLevel` at the model's declared dotted path. The
    // wire field varies by upstream dialect (`chat_template_kwargs.
    // enable_thinking` for vLLM/Qwen/GLM, `reasoning_effort` for
    // OpenAI-shaped routers), so the field + value live on the model, not
    // hand-wired here (doctrine 05: no provider matching at the seam). A
    // level absent from the declared map is not written — the model keeps
    // its upstream default. Anthropic/Responses providers ignore this and
    // keep using `thinking_value`.
    if let Some(rc) = &model.reasoning_control
        && let Some(value) = rc.values.get(&opts.thinking)
    {
        set_dotted(&mut body, &rc.field, value.clone());
    }

    // Tool definitions: sent as the native OpenAI `tools` array so the model
    // emits `tool_calls` instead of text-based fences. Like pi, we don't force
    // `tool_choice` — the model decides when to call tools vs. respond with
    // text. Forcing `required` traps the agent loop.
    if !ctx.tools.is_empty() {
        body["tools"] = Value::Array(ctx.tools.iter().map(openai_tool_schema).collect());
    }

    body
}

pub(super) fn openai_tool_schema(tool: &ToolDefinition) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": tool.input_schema,
        },
    })
}

/// Write `value` at a dotted `path` inside `body`, creating intermediate
/// objects as needed (`a.b.c` → `body[a][b][c] = value`). Used to place a
/// model-declared reasoning-control field (e.g.
/// `chat_template_kwargs.enable_thinking`) without the request builder
/// knowing the dialect.
fn set_dotted(body: &mut Value, path: &str, value: Value) {
    let parts: Vec<&str> = path.split('.').collect();
    let Some((last, ancestors)) = parts.split_last() else {
        return;
    };
    let mut cur = body;
    for part in ancestors {
        if cur.get(part).is_none() {
            cur[part] = json!({});
        }
        cur = cur.get_mut(part).expect("just created above");
    }
    cur[*last] = value;
}

pub(super) fn convert_messages(messages: &[Message], system_prompt: Option<&str>) -> Vec<Value> {
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{build_body, convert_messages, openai_tool_schema};
    use crate::types::{
        Api, AssistantContent, AssistantMessage, Auth, Context, CostRates, Message, Model,
        ReasoningControl, StreamOptions, ThinkingLevel, ToolDefinition, ToolResultMessage,
        UserMessage,
    };
    use serde_json::json;

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
            adaptive_thinking: false,
            reasoning_control: None,
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
    fn build_body_writes_reasoning_control_at_declared_path() {
        // A Completions model that declares its reasoning-control wire field
        // gets the value for the requested ThinkingLevel written at the
        // dotted path. The provider never matches on provider/model; it
        // echoes whatever the model declared.
        let mut m = model();
        m.reasoning = true;
        m.reasoning_control = Some(ReasoningControl {
            field: "chat_template_kwargs.enable_thinking".to_owned(),
            values: [
                (ThinkingLevel::Off, json!(false)),
                (ThinkingLevel::High, json!(true)),
            ]
            .into_iter()
            .collect(),
        });
        let ctx = Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage::new("hi"))],
            ..Default::default()
        };

        // Off → enable_thinking=false at the nested path.
        let mut opts = StreamOptions::new(Auth::ApiKey("k".to_owned()));
        opts.thinking = ThinkingLevel::Off;
        let body = build_body(&m, &ctx, &opts);
        assert_eq!(
            body["chat_template_kwargs"]["enable_thinking"],
            json!(false)
        );

        // High → enable_thinking=true.
        let mut opts = StreamOptions::new(Auth::ApiKey("k".to_owned()));
        opts.thinking = ThinkingLevel::High;
        let body = build_body(&m, &ctx, &opts);
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], json!(true));
    }

    #[test]
    fn build_body_skips_reasoning_control_for_undeclared_levels() {
        // A level absent from the declared `values` map is not written —
        // the model keeps its upstream default. (Matches the live probe:
        // GLM-5.2 reasons by default, so Medium-as-undeclared leaves the
        let mut m = model();
        m.reasoning = true;
        m.reasoning_control = Some(ReasoningControl {
            field: "chat_template_kwargs.enable_thinking".to_owned(),
            values: [(ThinkingLevel::Off, json!(false))].into_iter().collect(),
        });
        let ctx = Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage::new("hi"))],
            ..Default::default()
        };
        let mut opts = StreamOptions::new(Auth::ApiKey("k".to_owned()));
        opts.thinking = ThinkingLevel::Medium;
        let body = build_body(&m, &ctx, &opts);
        assert!(body.get("chat_template_kwargs").is_none());
    }

    #[test]
    fn build_body_no_reasoning_control_preserves_today_behavior() {
        // A model with no `reasoning_control` declaration sends nothing —
        // today's behavior, preserved. Kimi-K2.6 (no honored disable field
        // in the probe) rides this until its upstream is determined.
        let mut opts = StreamOptions::new(Auth::ApiKey("k".to_owned()));
        opts.thinking = ThinkingLevel::High;
        let ctx = Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage::new("hi"))],
            ..Default::default()
        };
        let body = build_body(&model(), &ctx, &opts);
        assert!(body.get("chat_template_kwargs").is_none());
        assert!(body.get("reasoning_effort").is_none());
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
        assistant
            .content
            .push(AssistantContent::ToolCall(crate::types::ToolCall {
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
        assert_eq!(
            out[0]["content"],
            "line1
line2"
        );
    }

    #[test]
    fn build_url_targets_chat_completions() {
        let m = model();
        let url = super::endpoint_url(&m);
        assert_eq!(url, "https://api.test.com/v1/chat/completions");
    }

    // --- Tool schema tests ---

    #[test]
    fn openai_tool_schema_nests_under_function() {
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
        let schema = openai_tool_schema(&tool);
        assert_eq!(schema["type"], "function");
        assert_eq!(schema["function"]["name"], "repl");
        assert_eq!(schema["function"]["description"], "Execute Python code");
        assert_eq!(schema["function"]["parameters"]["type"], "object");
        assert_eq!(
            schema["function"]["parameters"]["properties"]["code"]["type"],
            "string"
        );
        assert!(
            schema.get("name").is_none(),
            "completions schema must not have name at root"
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
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "repl");
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
    fn build_body_omits_tool_choice_when_tools_present() {
        // Like pi, we don't force tool_choice — the model decides when to
        // call tools. Forcing "required" traps the agent loop.
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
        assert!(body.get("tool_choice").is_none());
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

    // --- convert_messages with ToolResult ---

    #[test]
    fn convert_messages_serializes_tool_result_as_tool_role() {
        let result = ToolResultMessage::new("call_123", "42");
        let messages = vec![Message::ToolResult(result)];
        let out = convert_messages(&messages, None);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["role"], "tool");
        assert_eq!(out[0]["tool_call_id"], "call_123");
        assert_eq!(out[0]["content"], "42");
    }

    #[test]
    fn convert_messages_assistant_with_tool_calls_and_tool_result() {
        let mut assistant = AssistantMessage::empty(&model());
        assistant
            .content
            .push(AssistantContent::ToolCall(crate::types::ToolCall {
                id: "call_456".to_owned(),
                name: "repl".to_owned(),
                arguments: json!({"code": "print(1)"}),
            }));
        let result = ToolResultMessage::new("call_456", "1");
        let messages = vec![
            Message::Assistant(assistant),
            Message::ToolResult(result),
            Message::User(UserMessage::new("thanks")),
        ];
        let out = convert_messages(&messages, None);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0]["role"], "assistant");
        let tool_calls = out[0]["tool_calls"].as_array().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0]["id"], "call_456");
        assert_eq!(tool_calls[0]["function"]["name"], "repl");
        assert_eq!(
            tool_calls[0]["function"]["arguments"].as_str(),
            Some(r#"{"code":"print(1)"}"#)
        );
        assert_eq!(out[1]["role"], "tool");
        assert_eq!(out[1]["tool_call_id"], "call_456");
        assert_eq!(out[1]["content"], "1");
        assert_eq!(out[2]["role"], "user");
        assert_eq!(out[2]["content"], "thanks");
    }
}
