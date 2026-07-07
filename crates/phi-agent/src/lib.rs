//! phi-agent — the flat tool-call loop as a pack (DESIGN.md WS2).
//!
//! pi's shape: stream one assistant response, execute its tool calls
//! sequentially, append the results, repeat until a turn produces no tool
//! calls and no steering input. Provider errors and aborts land in the
//! transcript as the final assistant message — the loop returns them, it
//! never panics over them. The loop is headless: it emits [`AgentEvent`]s
//! for whichever frontend is attached and mirrors lifecycle events onto
//! the Lua host (only when someone subscribed).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use phi_ai::{
    AssistantEvent, AssistantEventStream, Context, Message, StopReason, ToolCall, ToolDefinition,
    ToolResultMessage, Usage, UserMessage,
};
use phi_host::Host;
use tokio_util::sync::CancellationToken;

pub mod prompt;
pub mod tools;
mod truncate;

/// One LLM request + the execution of its tool calls = one turn.
#[derive(Debug)]
pub enum AgentEvent {
    TurnStart {
        index: usize,
    },
    /// Streaming passthrough, including the terminal `Done`/`Error`.
    Assistant(AssistantEvent),
    ToolStart {
        id: String,
        name: String,
        arguments: serde_json::Value,
    },
    ToolEnd {
        id: String,
        name: String,
        content: String,
        is_error: bool,
    },
}

/// How a run ended. The transcript (including any error message) is in the
/// caller's `Context`; this is the summary.
#[derive(Debug)]
pub struct RunOutcome {
    pub stop_reason: StopReason,
    pub error_message: Option<String>,
    pub usage: Usage,
    pub turns: usize,
}

impl RunOutcome {
    pub fn is_error(&self) -> bool {
        matches!(self.stop_reason, StopReason::Error | StopReason::Aborted)
    }
}

/// Produces one streamed assistant response for the current context. The
/// app closes over `Registry::stream`; tests script it.
pub type StreamFn<'a> = dyn FnMut(&Context) -> AssistantEventStream + Send + 'a;

/// Messages injected while the agent is running, drained at turn
/// boundaries (pi's steering queue).
pub type SteeringQueue = Arc<Mutex<VecDeque<String>>>;

/// Run the flat loop until it settles. `ctx` accumulates the transcript
/// in place (the caller owns persistence). Cancellation is cooperative:
/// the token kills the stream (via `StreamOptions.cancel`, wired by the
/// caller's `stream_fn`) and running tools; unexecuted calls get error
/// results so the transcript stays well-formed.
pub async fn run_flat(
    host: &Arc<Host>,
    stream_fn: &mut StreamFn<'_>,
    ctx: &mut Context,
    cancel: &CancellationToken,
    steering: &SteeringQueue,
    on_event: &mut (dyn FnMut(AgentEvent) + Send),
) -> Result<RunOutcome> {
    let mut usage = Usage::default();
    let mut turns = 0;

    loop {
        // Re-read each turn: an extension may register tools mid-run.
        ctx.tools = tool_definitions(host);

        emit_lua(host, "turn_start", serde_json::json!({ "index": turns }));
        on_event(AgentEvent::TurnStart { index: turns });

        let mut stream = stream_fn(ctx);
        let message = loop {
            let event = stream
                .next()
                .await
                .ok_or_else(|| anyhow!("stream ended without a terminal event"))?;
            let terminal = event.terminal_message().cloned();
            on_event(AgentEvent::Assistant(event));
            if let Some(message) = terminal {
                break message;
            }
        };

        usage.add(&message.usage);
        turns += 1;
        let stop_reason = message.stop_reason;
        let error_message = message.error_message.clone();
        let calls: Vec<ToolCall> = message
            .content
            .iter()
            .filter_map(|block| match block {
                phi_ai::AssistantContent::ToolCall(call) => Some(call.clone()),
                _ => None,
            })
            .collect();
        ctx.messages.push(Message::Assistant(message));

        if matches!(stop_reason, StopReason::Error | StopReason::Aborted) {
            emit_lua(host, "agent_end", serde_json::json!({ "turns": turns }));
            return Ok(RunOutcome {
                stop_reason,
                error_message,
                usage,
                turns,
            });
        }

        for call in &calls {
            let output = execute_tool(host, call, cancel, on_event).await?;
            ctx.messages.push(Message::ToolResult(
                ToolResultMessage::new(call.id.clone(), output.content).with_error(output.is_error),
            ));
        }

        emit_lua(
            host,
            "turn_end",
            serde_json::json!({ "index": turns - 1, "tool_calls": calls.len() }),
        );

        if cancel.is_cancelled() {
            emit_lua(host, "agent_end", serde_json::json!({ "turns": turns }));
            return Ok(RunOutcome {
                stop_reason: StopReason::Aborted,
                error_message: None,
                usage,
                turns,
            });
        }

        let steered: Vec<String> = steering.lock().expect("steering queue").drain(..).collect();
        for text in &steered {
            ctx.messages
                .push(Message::User(UserMessage::new(text.clone())));
        }
        if calls.is_empty() && steered.is_empty() {
            break;
        }
    }

    emit_lua(host, "agent_end", serde_json::json!({ "turns": turns }));
    Ok(RunOutcome {
        stop_reason: StopReason::Stop,
        error_message: None,
        usage,
        turns,
    })
}

/// Execute one tool call: blockable via the `tool_call` event, cancellable,
/// and never a host error for tool-level failures (unknown tool included —
/// the model just gets an error result, pi's behavior).
async fn execute_tool(
    host: &Arc<Host>,
    call: &ToolCall,
    cancel: &CancellationToken,
    on_event: &mut (dyn FnMut(AgentEvent) + Send),
) -> Result<phi_host::ToolOutput> {
    on_event(AgentEvent::ToolStart {
        id: call.id.clone(),
        name: call.name.clone(),
        arguments: call.arguments.clone(),
    });

    let output = if cancel.is_cancelled() {
        phi_host::ToolOutput {
            content: "Aborted before execution".to_owned(),
            is_error: true,
        }
    } else if let Some(reason) = blocked_reason(host, call) {
        phi_host::ToolOutput {
            content: format!("Blocked by extension: {reason}"),
            is_error: true,
        }
    } else {
        let host = Arc::clone(host);
        let (name, args) = (call.name.clone(), call.arguments.clone());
        let result = tokio::task::spawn_blocking(move || host.invoke_tool(&name, &args))
            .await
            .map_err(|e| anyhow!("tool task panicked: {e}"))?;
        match result {
            Ok(output) => output,
            // Host-level failure (unknown tool, dead VM): error result.
            Err(e) => phi_host::ToolOutput {
                content: format!("{e:#}"),
                is_error: true,
            },
        }
    };

    on_event(AgentEvent::ToolEnd {
        id: call.id.clone(),
        name: call.name.clone(),
        content: output.content.clone(),
        is_error: output.is_error,
    });
    Ok(output)
}

/// Ask `tool_call` subscribers whether this call may run. Any handler
/// returning `{ block = true, reason? }` wins; handler errors fail open.
fn blocked_reason(host: &Host, call: &ToolCall) -> Option<String> {
    if host.subscriber_count("tool_call") == 0 {
        return None;
    }
    let payload = serde_json::json!({
        "tool_name": call.name,
        "tool_call_id": call.id,
        "input": call.arguments,
    });
    let outcomes = host.emit("tool_call", &payload).ok()?;
    outcomes.into_iter().find_map(|o| match o.result {
        Ok(Some(v)) if v.get("block").and_then(|b| b.as_bool()) == Some(true) => Some(
            v.get("reason")
                .and_then(|r| r.as_str())
                .unwrap_or("no reason given")
                .to_owned(),
        ),
        _ => None,
    })
}

fn tool_definitions(host: &Host) -> Vec<ToolDefinition> {
    host.tools()
        .into_iter()
        .map(|t| ToolDefinition {
            name: t.name,
            description: t.description,
            input_schema: t.input_schema,
        })
        .collect()
}

/// Mirror a lifecycle event onto the Lua bus, only when subscribed (an
/// unobserved emit is pure overhead). Handler outcomes are dropped here:
/// notify-only events.
fn emit_lua(host: &Host, event: &str, payload: serde_json::Value) {
    if host.subscriber_count(event) == 0 {
        return;
    }
    let _ = host.emit(event, &payload);
}
