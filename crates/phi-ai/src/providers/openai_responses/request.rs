use serde_json::{Value, json};

use crate::types::{AssistantContent, Context, Message, Model, StreamOptions, ToolDefinition};

pub(super) const CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";

pub(super) fn build_body(model: &Model, ctx: &Context, opts: &StreamOptions) -> Value {
    let chatgpt = matches!(opts.auth, crate::types::Auth::ChatGptOAuth { .. });
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
        // Cap to context_window defensively (see openai_completions::build_body).
        let max = opts
            .max_tokens
            .unwrap_or(model.max_tokens)
            .min(model.context_window);
        body["max_output_tokens"] = json!(max);
        if let Some(prompt) = &ctx.system_prompt {
            body["instructions"] = json!(prompt);
        }
    }

    if let Some(effort) = model.thinking_value(opts.thinking) {
        body["reasoning"] = json!({ "effort": effort, "summary": "auto" });
    } else if let Some(temperature) = opts.temperature {
        body["temperature"] = json!(temperature);
    }

    // Tool definitions: sent as the native OpenAI Responses `tools` array so
    // the model emits `function_call` items instead of text-based fences.
    // Like pi, we don't force `tool_choice` — the model decides when to call
    // tools vs. respond with text. Forcing `required` traps the agent loop
    // (it can never produce a tool-free turn to settle).
    if !ctx.tools.is_empty() {
        body["tools"] = Value::Array(ctx.tools.iter().map(openai_responses_tool_schema).collect());
    }

    body
}

pub(super) fn openai_responses_tool_schema(tool: &ToolDefinition) -> Value {
    json!({
        "type": "function",
        "name": tool.name,
        "description": tool.description,
        "parameters": tool.input_schema,
    })
}

pub(super) fn convert_messages(messages: &[Message]) -> Vec<Value> {
    let mut out = Vec::new();
    for message in messages {
        match message {
            Message::User(user) => out.push(json!({
                "role": "user",
                "content": [{ "type": "input_text", "text": user.content }],
            })),
            Message::Assistant(assistant) => {
                // Emit assistant text as a message item. Function calls are
                // emitted as separate top-level `function_call` items — the
                // Responses API does not accept `function_call` nested inside
                // a message's `content` array (HTTP 400 on replay).
                let text = assistant.text();
                if !text.is_empty() {
                    out.push(json!({
                        "role": "assistant",
                        "content": [{ "type": "output_text", "text": text }],
                    }));
                }
                for block in &assistant.content {
                    if let AssistantContent::ToolCall(call) = block {
                        out.push(json!({
                            "type": "function_call",
                            "call_id": call.id,
                            "name": call.name,
                            "arguments": serde_json::to_string(&call.arguments)
                                .unwrap_or_else(|_| "{}".to_owned()),
                        }));
                    }
                }
            }
            Message::ToolResult(result) => out.push(json!({
                "type": "function_call_output",
                "call_id": result.tool_use_id,
                "output": result.content,
            })),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{build_body, convert_messages, openai_responses_tool_schema};
    use crate::types::{
        Api, AssistantContent, AssistantMessage, Auth, Context, CostRates, Message, Model,
        StreamOptions, ToolDefinition, ToolResultMessage, UserMessage,
    };
    use serde_json::json;

    fn model() -> Model {
        Model {
            id: "gpt-test".to_owned(),
            name: "Test".to_owned(),
            api: Api::OpenAiResponses,
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
    fn openai_responses_tool_schema_has_name_at_root() {
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
        let schema = openai_responses_tool_schema(&tool);
        assert_eq!(schema["type"], "function");
        assert_eq!(schema["name"], "repl");
        assert_eq!(schema["description"], "Execute Python code");
        assert_eq!(schema["parameters"]["type"], "object");
        assert_eq!(schema["parameters"]["properties"]["code"]["type"], "string");
        assert!(
            schema.get("function").is_none(),
            "responses schema must not nest under function"
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
        assert_eq!(tools[0]["name"], "repl");
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
        assert!(body.get("tools").is_none(), "no tools = no tools key");
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

    #[test]
    fn convert_messages_serializes_assistant_tool_call() {
        // Function calls are top-level items, not nested inside the
        // assistant message content. Text and tool call produce two
        // separate top-level items in the output array.
        let mut assistant = AssistantMessage::empty(&model());
        assistant.content.push(AssistantContent::Text {
            text: "let me check".to_owned(),
        });
        assistant
            .content
            .push(AssistantContent::ToolCall(crate::types::ToolCall {
                id: "call_abc".to_owned(),
                name: "repl".to_owned(),
                arguments: json!({"code": "print(1)"}),
            }));
        let messages = vec![Message::Assistant(assistant)];
        let out = convert_messages(&messages);
        assert_eq!(out.len(), 2);
        // Text as an assistant message.
        assert_eq!(out[0]["role"], "assistant");
        let content = out[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "output_text");
        // Tool call as a top-level function_call item.
        assert_eq!(out[1]["type"], "function_call");
        assert_eq!(out[1]["call_id"], "call_abc");
        assert_eq!(out[1]["name"], "repl");
        assert_eq!(out[1]["arguments"].as_str(), Some(r#"{"code":"print(1)"}"#));
    }

    #[test]
    fn convert_messages_serializes_tool_result_as_function_call_output() {
        let result = ToolResultMessage::new("call_abc", "1");
        let messages = vec![Message::ToolResult(result)];
        let out = convert_messages(&messages);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["type"], "function_call_output");
        assert_eq!(out[0]["call_id"], "call_abc");
        assert_eq!(out[0]["output"], "1");
    }

    #[test]
    fn convert_messages_assistant_tool_call_then_tool_result() {
        let mut assistant = AssistantMessage::empty(&model());
        assistant
            .content
            .push(AssistantContent::ToolCall(crate::types::ToolCall {
                id: "call_def".to_owned(),
                name: "repl".to_owned(),
                arguments: json!({"code": "x=1"}),
            }));
        let result = ToolResultMessage::new("call_def", "ok");
        let messages = vec![
            Message::User(UserMessage::new("test")),
            Message::Assistant(assistant),
            Message::ToolResult(result),
        ];
        let out = convert_messages(&messages);
        // user, function_call (top-level), function_call_output = 3 items
        assert_eq!(out.len(), 3);
        assert_eq!(out[0]["role"], "user");
        assert_eq!(out[1]["type"], "function_call");
        assert_eq!(out[1]["call_id"], "call_def");
        assert_eq!(out[2]["type"], "function_call_output");
        assert_eq!(out[2]["call_id"], "call_def");
        assert_eq!(out[2]["output"], "ok");
    }

    #[test]
    fn convert_messages_serializes_null_arguments_as_string() {
        // A tool call whose arguments failed to parse (empty stream) becomes
        // Value::Null. The Responses API requires `arguments` to be a string,
        // never null — serializing it as JSON null triggers HTTP 400 on the
        // next turn. Must serialize as the string "null".
        let mut assistant = AssistantMessage::empty(&model());
        assistant
            .content
            .push(AssistantContent::ToolCall(crate::types::ToolCall {
                id: "call_null".to_owned(),
                name: "bash".to_owned(),
                arguments: serde_json::Value::Null,
            }));
        let messages = vec![Message::Assistant(assistant)];
        let out = convert_messages(&messages);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["type"], "function_call");
        assert_eq!(out[0]["call_id"], "call_null");
        assert_eq!(out[0]["arguments"].as_str(), Some("null"));
    }
}
