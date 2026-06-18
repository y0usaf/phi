use anyhow::Result;
use phi_ext::{EventKind, EventReturn, InputActionKind};

pub(super) fn decode_event_return(json_opt: Option<String>) -> Result<Option<EventReturn>> {
    let Some(s) = json_opt else {
        return Ok(None);
    };
    let v: serde_json::Value = serde_json::from_str(&s)?;
    let obj = v
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("event return: not an object"))?;

    if obj.contains_key("system_prompt_append") || obj.contains_key("system_prompt_prepend") {
        return Ok(Some(EventReturn::SystemPromptMod {
            append: obj
                .get("system_prompt_append")
                .and_then(|v| v.as_str())
                .map(str::to_owned),
            prepend: obj
                .get("system_prompt_prepend")
                .and_then(|v| v.as_str())
                .map(str::to_owned),
        }));
    }

    if obj.get("block").and_then(|v| v.as_bool()).unwrap_or(false) {
        let reason = obj
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("blocked")
            .to_owned();
        return Ok(Some(EventReturn::ToolBlock { reason }));
    }

    if obj.contains_key("result") {
        let content = obj
            .get("result")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        return Ok(Some(EventReturn::ToolResultOverride { content }));
    }

    if obj.contains_key("action") {
        let action = match obj.get("action").and_then(|v| v.as_str()) {
            Some("transform") => InputActionKind::Transform,
            Some("handled") => InputActionKind::Handled,
            _ => InputActionKind::Continue,
        };
        let text = obj
            .get("text")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        return Ok(Some(EventReturn::InputAction { action, text }));
    }

    if let Some(prepend_arr) = obj.get("prepend_messages").and_then(|v| v.as_array()) {
        let prepend = prepend_arr
            .iter()
            .filter_map(|item| {
                let role = item.get("role")?.as_str()?.to_owned();
                let content = item.get("content")?.as_str()?.to_owned();
                Some((role, content))
            })
            .collect();
        return Ok(Some(EventReturn::MessageInjection { prepend }));
    }

    Ok(None)
}

// ---------------------------------------------------------------------------
// parse_event_kind
// ---------------------------------------------------------------------------

pub(super) fn parse_event_kind(s: &str) -> Result<EventKind> {
    match s {
        "runtime_start" => Ok(EventKind::RuntimeStart),
        "session_start" => Ok(EventKind::SessionStart),
        "tui_ready" => Ok(EventKind::TuiReady),
        "agent_start" => Ok(EventKind::AgentStart),
        "agent_end" => Ok(EventKind::AgentEnd),
        "turn_start" => Ok(EventKind::TurnStart),
        "turn_end" => Ok(EventKind::TurnEnd),
        "before_agent_start" => Ok(EventKind::BeforeAgentStart),
        "context" => Ok(EventKind::Context),
        "tool_call" => Ok(EventKind::ToolCall),
        "tool_result" => Ok(EventKind::ToolResult),
        "input" => Ok(EventKind::Input),
        "session_shutdown" => Ok(EventKind::SessionShutdown),
        other => Err(anyhow::anyhow!("unknown event kind: {other}")),
    }
}
