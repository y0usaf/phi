//! Replay + request-shaping parity tests for `protocols::anthropic`
//! against the spec's `providers/anthropic.ts`, run over a loopback
//! raw-TCP HTTP server (sandbox-safe). Fixture provenance:
//! `tests/fixtures/anthropic/README.md`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use phi_ai::protocols::anthropic::{AnthropicOptions, stream_anthropic, stream_simple_anthropic};
use phi_ai::protocols::options::{SimpleStreamOptions, StreamOptions};
use phi_ai_types::{
    AssistantMessage, AssistantMessageEvent, Context, Model, ThinkingLevel, Usage, calculate_cost,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// ---------------------------------------------------------------------
// Loopback SSE server with request capture
// ---------------------------------------------------------------------

type Captured = Arc<Mutex<Vec<String>>>;

fn sse_response(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

/// Serve one canned response per connection, capturing raw requests.
fn serve(responses: Vec<String>) -> (SocketAddr, Captured) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let listener = tokio::net::TcpListener::from_std(listener).unwrap();
    let captured: Captured = Arc::new(Mutex::new(Vec::new()));
    let capture = Arc::clone(&captured);
    tokio::spawn(async move {
        let mut responses = responses.into_iter();
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(conn) => conn,
                Err(_) => return,
            };
            let request = read_request(&mut sock).await;
            capture.lock().unwrap().push(request);
            match responses.next() {
                Some(response) => {
                    let _ = sock.write_all(response.as_bytes()).await;
                    let _ = sock.shutdown().await;
                }
                None => return,
            }
        }
    });
    (addr, captured)
}

/// Read one HTTP request: headers plus a content-length body.
async fn read_request(sock: &mut tokio::net::TcpStream) -> String {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    loop {
        let n = match sock.read(&mut tmp).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..pos]).to_lowercase();
            let content_length: usize = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
            while buf.len() - (pos + 4) < content_length {
                let n = match sock.read(&mut tmp).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
            }
            break;
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

fn request_header<'a>(request: &'a str, name: &str) -> Option<&'a str> {
    let head = request.split("\r\n\r\n").next().unwrap_or("");
    head.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        (key.eq_ignore_ascii_case(name)).then_some(value.trim())
    })
}

fn request_body(request: &str) -> Value {
    let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
    serde_json::from_str(body).unwrap()
}

// ---------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------

fn fixture(name: &str) -> String {
    let path = format!(
        "{}/tests/fixtures/anthropic/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(path).unwrap()
}

fn opus_model(addr: SocketAddr) -> Model {
    serde_json::from_value(json!({
        "id": "claude-opus-4-7",
        "name": "Claude Opus 4.7",
        "api": "anthropic-messages",
        "provider": "anthropic",
        "baseUrl": format!("http://{addr}"),
        "compat": { "forceAdaptiveThinking": true, "supportsTemperature": false },
        "reasoning": true,
        "thinkingLevelMap": { "xhigh": "xhigh" },
        "input": ["text", "image"],
        "cost": { "input": 5, "output": 25, "cacheRead": 0.5, "cacheWrite": 6.25 },
        "contextWindow": 1000000,
        "maxTokens": 128000
    }))
    .unwrap()
}

fn sonnet_model(addr: SocketAddr) -> Model {
    serde_json::from_value(json!({
        "id": "claude-sonnet-4-5",
        "name": "Claude Sonnet 4.5",
        "api": "anthropic-messages",
        "provider": "anthropic",
        "baseUrl": format!("http://{addr}"),
        "reasoning": true,
        "input": ["text", "image"],
        "cost": { "input": 3, "output": 15, "cacheRead": 0.3, "cacheWrite": 3.75 },
        "contextWindow": 1000000,
        "maxTokens": 64000
    }))
    .unwrap()
}

fn user_context(text: &str) -> Context {
    serde_json::from_value(json!({
        "messages": [{ "role": "user", "content": text, "timestamp": 1 }]
    }))
    .unwrap()
}

fn api_key_options(key: &str) -> AnthropicOptions {
    AnthropicOptions {
        base: StreamOptions {
            api_key: Some(key.to_string()),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Drain the stream: (events, final message from `result()`).
async fn collect(
    stream: &phi_ai::transport::AssistantMessageEventStream,
) -> (Vec<AssistantMessageEvent>, Option<AssistantMessage>) {
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }
    let result = stream.result().await;
    (events, result)
}

/// Event JSON minus the `partial`/`message`/`error` snapshots.
fn event_summary(event: &AssistantMessageEvent) -> Value {
    let mut value = serde_json::to_value(event).unwrap();
    if let Some(map) = value.as_object_mut() {
        map.remove("partial");
        map.remove("message");
        map.remove("error");
    }
    value
}

/// A minimal complete transcript ending with `end_turn`.
fn minimal_transcript(stop_reason: &str) -> String {
    format!(
        concat!(
            "event: message_start\n",
            "data: {{\"type\":\"message_start\",\"message\":{{\"id\":\"msg_1\",\"usage\":{{\"input_tokens\":10,\"output_tokens\":1}}}}}}\n\n",
            "event: content_block_start\n",
            "data: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"text\",\"text\":\"\"}}}}\n\n",
            "event: content_block_delta\n",
            "data: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"ok\"}}}}\n\n",
            "event: content_block_stop\n",
            "data: {{\"type\":\"content_block_stop\",\"index\":0}}\n\n",
            "event: message_delta\n",
            "data: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"{stop}\"}},\"usage\":{{\"output_tokens\":2}}}}\n\n",
            "event: message_stop\n",
            "data: {{\"type\":\"message_stop\"}}\n\n",
        ),
        stop = stop_reason
    )
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[tokio::test]
async fn replays_text_thinking_and_tool_use() {
    let (addr, captured) = serve(vec![sse_response(&fixture("replay_basic.sse"))]);
    let model = opus_model(addr);

    let stream = stream_anthropic(
        &model,
        &user_context("hi"),
        Some(api_key_options("sk-test")),
    );
    let (events, result) = collect(&stream).await;

    // Event sequence parity.
    let summaries: Vec<Value> = events.iter().map(event_summary).collect();
    let expected_events: Value =
        serde_json::from_str(&fixture("replay_basic.events.json")).unwrap();
    assert_eq!(Value::Array(summaries), expected_events);

    // Final message parity (timestamp normalized, cost derived).
    let mut message = result.unwrap();
    message.timestamp = 0;
    let mut expected: Value = serde_json::from_str(&fixture("replay_basic.message.json")).unwrap();
    let mut usage: Usage = serde_json::from_value(expected["usage"].clone()).unwrap();
    let cost = calculate_cost(&model, &mut usage);
    expected["usage"]["cost"] = serde_json::to_value(cost).unwrap();
    assert_eq!(serde_json::to_value(&message).unwrap(), expected);

    // SDK-reproduced request surface: URL path, version header, API key.
    let request = captured.lock().unwrap().remove(0);
    assert!(request.starts_with("POST /v1/messages HTTP/1.1\r\n"));
    assert_eq!(
        request_header(&request, "anthropic-version"),
        Some("2023-06-01")
    );
    assert_eq!(request_header(&request, "x-api-key"), Some("sk-test"));
    assert_eq!(request_header(&request, "authorization"), None);
}

#[tokio::test]
async fn builds_full_api_key_params() {
    let (addr, captured) = serve(vec![sse_response(&minimal_transcript("end_turn"))]);
    let model = opus_model(addr);

    let context: Context = serde_json::from_value(json!({
        "systemPrompt": "You are helpful.",
        "messages": [
            { "role": "user", "content": "Hello", "timestamp": 1 },
            {
                "role": "assistant",
                "content": [
                    { "type": "thinking", "thinking": "Consider the file.", "thinkingSignature": "sig1" },
                    { "type": "text", "text": "Reading." },
                    { "type": "toolCall", "id": "toolu_1", "name": "read", "arguments": { "path": "a.txt" } }
                ],
                "api": "anthropic-messages",
                "provider": "anthropic",
                "model": "claude-opus-4-7",
                "usage": {
                    "input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 2,
                    "cost": { "input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0, "total": 0.0 }
                },
                "stopReason": "toolUse",
                "timestamp": 1
            },
            {
                "role": "toolResult",
                "toolCallId": "toolu_1",
                "toolName": "read",
                "content": [{ "type": "text", "text": "file contents" }],
                "isError": false,
                "timestamp": 1
            },
            { "role": "user", "content": "Continue", "timestamp": 1 }
        ],
        "tools": [{
            "name": "read",
            "description": "Read a file",
            "parameters": {
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }
        }]
    }))
    .unwrap();

    let mut options = api_key_options("sk-test");
    options.base.temperature = Some(0.5);
    options.base.session_id = Some("sess-1".to_string());
    options.base.metadata = Some(
        json!({ "user_id": "abc", "ignored": 123 })
            .as_object()
            .cloned()
            .unwrap(),
    );
    options.thinking_enabled = Some(true);
    options.effort = Some("high".to_string());
    options.tool_choice = Some(phi_ai::protocols::anthropic::AnthropicToolChoice::Auto);

    let stream = stream_anthropic(&model, &context, Some(options));
    let (_events, result) = collect(&stream).await;
    assert_eq!(result.unwrap().error_message, None);

    let request = captured.lock().unwrap().remove(0);
    let expected: Value = serde_json::from_str(&fixture("params_apikey.request.json")).unwrap();
    assert_eq!(request_body(&request), expected);
    // Session affinity is off by default for anthropic; adaptive models
    // skip the interleaved beta and eager streaming needs no beta.
    assert_eq!(request_header(&request, "x-session-affinity"), None);
    assert_eq!(request_header(&request, "anthropic-beta"), None);
}

#[tokio::test]
async fn oauth_requests_use_claude_code_identity() {
    let transcript = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_2\",\"usage\":{\"input_tokens\":5,\"output_tokens\":1}}}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_9\",\"name\":\"Read\",\"input\":{}}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"x\\\"}\"}}\n\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":3}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );
    let (addr, captured) = serve(vec![sse_response(transcript)]);
    let model = opus_model(addr);

    let context: Context = serde_json::from_value(json!({
        "systemPrompt": "Be brief.",
        "messages": [{ "role": "user", "content": "read x", "timestamp": 1 }],
        "tools": [{
            "name": "read",
            "description": "Read a file",
            "parameters": { "type": "object", "properties": {}, "required": [] }
        }]
    }))
    .unwrap();

    let stream = stream_anthropic(&model, &context, Some(api_key_options("sk-ant-oat01-xyz")));
    let (_events, result) = collect(&stream).await;
    let message = result.unwrap();
    assert_eq!(message.error_message, None);

    // Response tool name maps back through the registered tools.
    let content = serde_json::to_value(&message.content).unwrap();
    assert_eq!(content[0]["name"], "read");
    assert_eq!(content[0]["arguments"], json!({ "path": "x" }));

    let request = captured.lock().unwrap().remove(0);
    assert_eq!(
        request_header(&request, "authorization"),
        Some("Bearer sk-ant-oat01-xyz")
    );
    assert_eq!(request_header(&request, "x-api-key"), None);
    assert_eq!(
        request_header(&request, "anthropic-beta"),
        Some("claude-code-20250219,oauth-2025-04-20")
    );
    assert_eq!(
        request_header(&request, "user-agent"),
        Some("claude-cli/2.1.75")
    );
    assert_eq!(request_header(&request, "x-app"), Some("cli"));

    // OAuth payload: Claude Code identity first, then the system prompt;
    // tool names in CC canonical casing.
    let body = request_body(&request);
    assert_eq!(
        body["system"][0]["text"],
        "You are Claude Code, Anthropic's official CLI for Claude."
    );
    assert_eq!(body["system"][1]["text"], "Be brief.");
    assert_eq!(body["tools"][0]["name"], "Read");
}

#[tokio::test]
async fn simple_reasoning_maps_to_budget_thinking() {
    let (addr, captured) = serve(vec![sse_response(&minimal_transcript("end_turn"))]);
    let model = sonnet_model(addr);

    let options = SimpleStreamOptions {
        base: StreamOptions {
            api_key: Some("sk-test".to_string()),
            ..Default::default()
        },
        reasoning: Some(ThinkingLevel::High),
        thinking_budgets: None,
    };
    let stream = stream_simple_anthropic(&model, &user_context("hi"), Some(options)).unwrap();
    let (_events, result) = collect(&stream).await;
    assert_eq!(result.unwrap().error_message, None);

    let request = captured.lock().unwrap().remove(0);
    let body = request_body(&request);
    assert_eq!(body["max_tokens"], json!(64000));
    assert_eq!(
        body["thinking"],
        json!({ "type": "enabled", "budget_tokens": 16384, "display": "summarized" })
    );
    // Non-adaptive model: interleaved-thinking beta requested by default.
    assert_eq!(
        request_header(&request, "anthropic-beta"),
        Some("interleaved-thinking-2025-05-14")
    );
}

#[tokio::test]
async fn simple_reasoning_maps_to_adaptive_effort() {
    let (addr, captured) = serve(vec![sse_response(&minimal_transcript("end_turn"))]);
    let model = opus_model(addr);

    let options = SimpleStreamOptions {
        base: StreamOptions {
            api_key: Some("sk-test".to_string()),
            ..Default::default()
        },
        reasoning: Some(ThinkingLevel::XHigh),
        thinking_budgets: None,
    };
    let stream = stream_simple_anthropic(&model, &user_context("hi"), Some(options)).unwrap();
    let (_events, result) = collect(&stream).await;
    assert_eq!(result.unwrap().error_message, None);

    let request = captured.lock().unwrap().remove(0);
    let body = request_body(&request);
    assert_eq!(
        body["thinking"],
        json!({ "type": "adaptive", "display": "summarized" })
    );
    assert_eq!(body["output_config"], json!({ "effort": "xhigh" }));
    assert_eq!(body.get("temperature"), None);
}

#[tokio::test]
async fn sse_error_events_become_error_messages() {
    let data = r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
    let transcript = format!("event: error\ndata: {data}\n\n");
    let (addr, _captured) = serve(vec![sse_response(&transcript)]);
    let model = opus_model(addr);

    let stream = stream_anthropic(
        &model,
        &user_context("hi"),
        Some(api_key_options("sk-test")),
    );
    let (events, result) = collect(&stream).await;
    let message = result.unwrap();
    assert_eq!(serde_json::to_value(message.stop_reason).unwrap(), "error");
    assert_eq!(message.error_message.as_deref(), Some(data));
    assert!(matches!(
        events.last(),
        Some(AssistantMessageEvent::Error { .. })
    ));
}

#[tokio::test]
async fn truncated_streams_error_before_message_stop() {
    let transcript = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_3\",\"usage\":{\"input_tokens\":5,\"output_tokens\":1}}}\n\n",
    );
    let (addr, _captured) = serve(vec![sse_response(transcript)]);
    let model = opus_model(addr);

    let stream = stream_anthropic(
        &model,
        &user_context("hi"),
        Some(api_key_options("sk-test")),
    );
    let (_events, result) = collect(&stream).await;
    assert_eq!(
        result.unwrap().error_message.as_deref(),
        Some("Anthropic stream ended before message_stop")
    );
}

#[tokio::test]
async fn unknown_stop_reasons_error() {
    let (addr, _captured) = serve(vec![sse_response(&minimal_transcript("banana"))]);
    let model = opus_model(addr);

    let stream = stream_anthropic(
        &model,
        &user_context("hi"),
        Some(api_key_options("sk-test")),
    );
    let (_events, result) = collect(&stream).await;
    assert_eq!(
        result.unwrap().error_message.as_deref(),
        Some("Unhandled stop reason: banana")
    );
}

#[tokio::test]
async fn missing_api_key_is_an_error() {
    let addr: SocketAddr = "127.0.0.1:1".parse().unwrap();
    let model = opus_model(addr);

    // streamAnthropic: folded into the error event.
    let stream = stream_anthropic(&model, &user_context("hi"), None);
    let (_events, result) = collect(&stream).await;
    assert_eq!(
        result.unwrap().error_message.as_deref(),
        Some("No API key for provider: anthropic")
    );

    // streamSimpleAnthropic: the spec throws synchronously.
    let error = stream_simple_anthropic(&model, &user_context("hi"), None)
        .err()
        .unwrap();
    assert_eq!(error.to_string(), "No API key for provider: anthropic");
}
