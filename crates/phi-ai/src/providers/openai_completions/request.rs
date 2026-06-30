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

    // Tool definitions: sent as the native OpenAI `tools` array so the model
    // emits `tool_calls` instead of text-based fences.
    if !ctx.tools.is_empty() {
        body["tools"] = Value::Array(ctx.tools.iter().map(openai_tool_schema).collect());
        // Force the model to call a tool every turn. Without this, reasoning
        // models default to `auto` and loop in prose instead of emitting
        // `tool_calls`. OpenAI's Chat Completions API supports
        // `tool_choice: "required"` alongside reasoning with no conflict.
        body["tool_choice"] = json!("required");
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
        StreamOptions, ToolDefinition, ToolResultMessage, UserMessage,
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
    fn build_body_forces_tool_choice_required_when_tools_present() {
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
        assert_eq!(body["tool_choice"], "required");
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
