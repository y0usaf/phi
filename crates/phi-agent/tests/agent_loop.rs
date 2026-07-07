//! WS2.1 acceptance: the flat loop against a scripted stream — tool
//! round-trips, steering, error stops, blocking via the `tool_call` event,
//! and abort semantics. No network, no real model.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use phi_agent::{AgentEvent, SteeringQueue, run_flat};
use phi_ai::{
    Api, AssistantContent, AssistantEvent, AssistantMessage, Context, Message, Model, StopReason,
    ToolCall,
};
use phi_host::Host;
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn model() -> Model {
    Model {
        id: "fake".into(),
        name: "Fake".into(),
        api: Api::AnthropicMessages,
        provider: "test".into(),
        base_url: String::new(),
        reasoning: false,
        input: Vec::new(),
        cost: Default::default(),
        context_window: 200_000,
        max_tokens: 8192,
        headers: Default::default(),
        thinking_levels: None,
        adaptive_thinking: false,
    }
}

fn text_message(text: &str) -> AssistantMessage {
    let mut m = AssistantMessage::empty(&model());
    m.content.push(AssistantContent::Text { text: text.into() });
    m
}

fn tool_message(id: &str, name: &str, args: serde_json::Value) -> AssistantMessage {
    let mut m = AssistantMessage::empty(&model());
    m.content.push(AssistantContent::ToolCall(ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: args,
    }));
    m.stop_reason = StopReason::ToolUse;
    m
}

/// A stream_fn that pops one scripted message per call.
fn scripted(
    messages: Vec<AssistantMessage>,
) -> impl FnMut(&Context) -> phi_ai::AssistantEventStream + Send {
    let queue = Arc::new(Mutex::new(VecDeque::from(messages)));
    move |_ctx| {
        let (sink, stream) = phi_ai::AssistantEventStream::channel();
        let message = queue
            .lock()
            .unwrap()
            .pop_front()
            .expect("scripted stream exhausted");
        if message.is_error() {
            sink.push(AssistantEvent::Error { message });
        } else {
            sink.push(AssistantEvent::Done { message });
        }
        stream
    }
}

fn run(
    host: &Arc<Host>,
    script: Vec<AssistantMessage>,
    ctx: &mut Context,
) -> (phi_agent::RunOutcome, Vec<String>) {
    let cancel = CancellationToken::new();
    let steering: SteeringQueue = Arc::default();
    run_with(host, script, ctx, &cancel, &steering)
}

fn run_with(
    host: &Arc<Host>,
    script: Vec<AssistantMessage>,
    ctx: &mut Context,
    cancel: &CancellationToken,
    steering: &SteeringQueue,
) -> (phi_agent::RunOutcome, Vec<String>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let events2 = Arc::clone(&events);
    let mut stream_fn = scripted(script);
    let outcome = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(run_flat(
            host,
            &mut stream_fn,
            ctx,
            cancel,
            steering,
            &mut move |event| {
                let tag = match event {
                    AgentEvent::TurnStart { index } => format!("turn_start:{index}"),
                    AgentEvent::Assistant(_) => "assistant".into(),
                    AgentEvent::ToolStart { name, .. } => format!("tool_start:{name}"),
                    AgentEvent::ToolEnd { name, is_error, .. } => {
                        format!("tool_end:{name}:{}", if is_error { "err" } else { "ok" })
                    }
                };
                events2.lock().unwrap().push(tag);
            },
        ))
        .expect("run_flat");
    let events = events.lock().unwrap().clone();
    (outcome, events)
}

fn host_with_echo_tool() -> Arc<Host> {
    let host = Host::new().expect("host");
    host.register_tool(
        "echo",
        "Echo the input back.",
        json!({ "type": "object", "properties": { "text": { "type": "string" } } }),
        "<test>",
        Box::new(|args| Ok(format!("echo: {}", args["text"].as_str().unwrap_or("")))),
    )
    .unwrap();
    Arc::new(host)
}

#[test]
fn text_only_response_ends_after_one_turn() {
    let host = Arc::new(Host::new().unwrap());
    let mut ctx = Context::default();
    let (outcome, events) = run(&host, vec![text_message("done")], &mut ctx);
    assert_eq!(outcome.turns, 1);
    assert!(matches!(outcome.stop_reason, StopReason::Stop));
    assert_eq!(events, vec!["turn_start:0", "assistant"]);
    assert_eq!(ctx.messages.len(), 1);
}

#[test]
fn tool_call_round_trips_and_loop_continues() {
    let host = host_with_echo_tool();
    let mut ctx = Context::default();
    let (outcome, events) = run(
        &host,
        vec![
            tool_message("c1", "echo", json!({ "text": "hi" })),
            text_message("done"),
        ],
        &mut ctx,
    );
    assert_eq!(outcome.turns, 2);
    assert_eq!(
        events,
        vec![
            "turn_start:0",
            "assistant",
            "tool_start:echo",
            "tool_end:echo:ok",
            "turn_start:1",
            "assistant"
        ]
    );
    // Transcript: assistant(tool_use), tool_result, assistant(text).
    assert_eq!(ctx.messages.len(), 3);
    let Message::ToolResult(result) = &ctx.messages[1] else {
        panic!("expected tool result, got {:?}", ctx.messages[1]);
    };
    assert_eq!(result.tool_use_id, "c1");
    assert_eq!(result.content, "echo: hi");
    assert!(!result.is_error);
}

#[test]
fn unknown_tool_becomes_error_result_not_crash() {
    let host = Arc::new(Host::new().unwrap());
    let mut ctx = Context::default();
    let (outcome, events) = run(
        &host,
        vec![
            tool_message("c1", "nope", json!({})),
            text_message("recovered"),
        ],
        &mut ctx,
    );
    assert_eq!(outcome.turns, 2);
    assert!(
        events.contains(&"tool_end:nope:err".to_owned()),
        "{events:?}"
    );
    let Message::ToolResult(result) = &ctx.messages[1] else {
        panic!("expected tool result");
    };
    assert!(result.is_error);
    assert!(
        result.content.contains("unknown tool"),
        "{}",
        result.content
    );
}

#[test]
fn provider_error_stops_the_run() {
    let host = Arc::new(Host::new().unwrap());
    let mut ctx = Context::default();
    let error = AssistantMessage::error(&model(), StopReason::Error, "rate limited");
    let (outcome, _) = run(&host, vec![error], &mut ctx);
    assert!(outcome.is_error());
    assert_eq!(outcome.error_message.as_deref(), Some("rate limited"));
    assert_eq!(
        ctx.messages.len(),
        1,
        "error message still lands in transcript"
    );
}

#[test]
fn steering_message_keeps_the_loop_going() {
    let host = Arc::new(Host::new().unwrap());
    let mut ctx = Context::default();
    let cancel = CancellationToken::new();
    let steering: SteeringQueue = Arc::default();
    steering.lock().unwrap().push_back("also do X".to_owned());
    let (outcome, _) = run_with(
        &host,
        vec![text_message("first"), text_message("second")],
        &mut ctx,
        &cancel,
        &steering,
    );
    assert_eq!(outcome.turns, 2);
    // assistant, injected user, assistant.
    assert_eq!(ctx.messages.len(), 3);
    assert!(matches!(&ctx.messages[1], Message::User(u) if u.content == "also do X"));
}

#[test]
fn lua_handler_blocks_a_tool_call() {
    let host = host_with_echo_tool();
    host.load_source(
        "gate.lua",
        r#"
        phi.on("tool_call", function(ev)
          if ev.tool_name == "echo" then
            return { block = true, reason = "echo is forbidden" }
          end
        end)
        "#,
    )
    .unwrap();
    let mut ctx = Context::default();
    let (_, events) = run(
        &host,
        vec![
            tool_message("c1", "echo", json!({ "text": "hi" })),
            text_message("ok"),
        ],
        &mut ctx,
    );
    assert!(
        events.contains(&"tool_end:echo:err".to_owned()),
        "{events:?}"
    );
    let Message::ToolResult(result) = &ctx.messages[1] else {
        panic!("expected tool result");
    };
    assert!(
        result.content.contains("echo is forbidden"),
        "{}",
        result.content
    );
}

#[test]
fn cancelled_run_marks_remaining_tools_aborted() {
    let host = host_with_echo_tool();
    let mut ctx = Context::default();
    let cancel = CancellationToken::new();
    cancel.cancel();
    let steering: SteeringQueue = Arc::default();
    let (outcome, _) = run_with(
        &host,
        vec![tool_message("c1", "echo", json!({ "text": "hi" }))],
        &mut ctx,
        &cancel,
        &steering,
    );
    assert!(matches!(outcome.stop_reason, StopReason::Aborted));
    let Message::ToolResult(result) = &ctx.messages[1] else {
        panic!("expected tool result");
    };
    assert!(result.is_error);
    assert!(result.content.contains("Aborted"), "{}", result.content);
}

#[test]
fn turn_events_reach_lua_subscribers() {
    let host = host_with_echo_tool();
    host.load_source(
        "trace.lua",
        r#"
        turns = 0
        phi.on("turn_start", function() turns = turns + 1 end)
        phi.on("agent_end", function()
          phi.register_command({ name = "turns", handler = function() return tostring(turns) end })
        end)
        "#,
    )
    .unwrap();
    let mut ctx = Context::default();
    run(
        &host,
        vec![
            tool_message("c1", "echo", json!({ "text": "x" })),
            text_message("done"),
        ],
        &mut ctx,
    );
    let result = host.invoke_command("turns", "").unwrap();
    assert_eq!(result, Some(json!({ "message": "2" })));
}
