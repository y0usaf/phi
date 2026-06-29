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

    // System prompt modification
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

    // Tool block
    if obj.get("block").and_then(|v| v.as_bool()).unwrap_or(false) {
        let reason = obj
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("blocked")
            .to_owned();
        return Ok(Some(EventReturn::ToolBlock { reason }));
    }

    // Tool result override
    if obj.contains_key("result") {
        let content = obj
            .get("result")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        return Ok(Some(EventReturn::ToolResultOverride { content }));
    }

    // Input action
    if obj.contains_key("action") {
        let action = match obj.get("action").and_then(|v| v.as_str()) {
            Some("transform") => InputActionKind::Transform,
            Some("handled") => InputActionKind::Handled,
            _ => InputActionKind::Continue,
        };
        let text = obj.get("text").and_then(|v| v.as_str()).map(str::to_owned);
        return Ok(Some(EventReturn::InputAction { action, text }));
    }

    // Message injection (prepend messages)
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

    // Cancel (session interception)
    if obj.get("cancel").and_then(|v| v.as_bool()).unwrap_or(false) {
        let reason = obj
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("cancelled")
            .to_owned();
        return Ok(Some(EventReturn::Cancel { reason }));
    }

    // Provider request override
    if obj.contains_key("provider_payload") {
        let payload = obj
            .get("provider_payload")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        return Ok(Some(EventReturn::ProviderRequestOverride { payload }));
    }

    // Resources discovered
    if obj.contains_key("skill_paths")
        || obj.contains_key("prompt_paths")
        || obj.contains_key("theme_paths")
    {
        let skill_paths = obj
            .get("skill_paths")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let prompt_paths = obj
            .get("prompt_paths")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let theme_paths = obj
            .get("theme_paths")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        return Ok(Some(EventReturn::ResourcesDiscovered {
            skill_paths,
            prompt_paths,
            theme_paths,
        }));
    }

    // Project trust result
    if obj.contains_key("trusted") {
        let trusted = obj.get("trusted").and_then(|v| v.as_bool()).unwrap_or(true);
        let remember = obj
            .get("remember")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        return Ok(Some(EventReturn::ProjectTrustResult { trusted, remember }));
    }

    // Compact custom instructions
    if obj.contains_key("compact_instructions") {
        let instructions = obj
            .get("compact_instructions")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        return Ok(Some(EventReturn::CompactCustom { instructions }));
    }

    // Message replace
    if obj.contains_key("replace_message") {
        let message = obj
            .get("replace_message")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        return Ok(Some(EventReturn::MessageReplace { message }));
    }

    Ok(None)
}

// ---------------------------------------------------------------------------
// parse_event_kind
// ---------------------------------------------------------------------------

pub(super) fn parse_event_kind(s: &str) -> Result<EventKind> {
    match s {
        // Core lifecycle
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
        // Project trust
        "project_trust" => Ok(EventKind::ProjectTrust),
        // Resource discovery
        "resources_discover" => Ok(EventKind::ResourcesDiscover),
        // Session interception
        "session_before_switch" => Ok(EventKind::SessionBeforeSwitch),
        "session_before_fork" => Ok(EventKind::SessionBeforeFork),
        "session_before_compact" => Ok(EventKind::SessionBeforeCompact),
        "session_compact" => Ok(EventKind::SessionCompact),
        "session_before_tree" => Ok(EventKind::SessionBeforeTree),
        "session_tree" => Ok(EventKind::SessionTree),
        // Provider events
        "before_provider_request" => Ok(EventKind::BeforeProviderRequest),
        "after_provider_response" => Ok(EventKind::AfterProviderResponse),
        // Message streaming
        "message_start" => Ok(EventKind::MessageStart),
        "message_update" => Ok(EventKind::MessageUpdate),
        "message_end" => Ok(EventKind::MessageEnd),
        // Tool execution
        "tool_execution_start" => Ok(EventKind::ToolExecutionStart),
        "tool_execution_update" => Ok(EventKind::ToolExecutionUpdate),
        "tool_execution_end" => Ok(EventKind::ToolExecutionEnd),
        // Model selection
        "model_select" => Ok(EventKind::ModelSelect),
        "thinking_level_select" => Ok(EventKind::ThinkingLevelSelect),
        // User bash
        "user_bash" => Ok(EventKind::UserBash),
        other => Err(anyhow::anyhow!("unknown event kind: {other}")),
    }
}
