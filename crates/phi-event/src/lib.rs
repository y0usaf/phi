use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The kind of lifecycle event that was emitted.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum EventKind {
    RuntimeStart,
    SessionStart,
    TuiReady,
    AgentStart,
    AgentEnd,
    TurnStart,
    TurnEnd,
    BeforeAgentStart,
    Context,
    ToolCall,
    ToolResult,
    Input,
    SessionShutdown,
}

/// The payload accompanying a lifecycle event.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum EventPayload {
    Empty,
    SessionStart {
        cwd: String,
        session_id: String,
    },
    BeforeAgentStart {
        prompt: String,
        system_prompt: String,
    },
    Context {
        turn: u32,
    },
    TurnStart {
        turn: u32,
    },
    TurnEnd {
        turn: u32,
        had_code: bool,
    },
    AgentEnd {
        stop_cause: String,
    },
    ToolCall {
        name: String,
        args: Value,
    },
    ToolResult {
        name: String,
        result: String,
        error: Option<String>,
    },
    Input {
        text: String,
    },
}

/// A lifecycle event dispatched to extension handlers.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LifecycleEvent {
    pub kind: EventKind,
    pub payload: EventPayload,
}

/// The value returned by an extension event handler.
#[derive(Clone, Debug)]
pub enum EventReturn {
    SystemPromptMod {
        append: Option<String>,
        prepend: Option<String>,
    },
    MessageInjection {
        prepend: Vec<(String, String)>,
    },
    ToolBlock {
        reason: String,
    },
    ToolResultOverride {
        content: String,
    },
    InputAction {
        action: InputActionKind,
        text: Option<String>,
    },
}

/// The kind of input action requested by an event handler.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum InputActionKind {
    Continue,
    Transform,
    Handled,
}

/// A handler function invoked when a lifecycle event is dispatched.
pub type EventHandler = Arc<dyn Fn(LifecycleEvent) -> Result<Option<EventReturn>> + Send + Sync>;

#[derive(Deserialize)]
pub struct EventHandlerRegistration {
    pub event: EventKind,
    pub label: String,
    #[serde(skip, default = "default_handler")]
    pub handler: EventHandler,
}

fn default_handler() -> EventHandler {
    Arc::new(|_event| Ok(None))
}

impl std::fmt::Display for EventKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::RuntimeStart => "runtime_start",
            Self::SessionStart => "session_start",
            Self::TuiReady => "tui_ready",
            Self::AgentStart => "agent_start",
            Self::AgentEnd => "agent_end",
            Self::TurnStart => "turn_start",
            Self::TurnEnd => "turn_end",
            Self::BeforeAgentStart => "before_agent_start",
            Self::Context => "context",
            Self::ToolCall => "tool_call",
            Self::ToolResult => "tool_result",
            Self::Input => "input",
            Self::SessionShutdown => "session_shutdown",
        };

        f.write_str(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // --- EventKind Display tests ---

    #[test]
    fn test_event_kind_display() {
        assert_eq!(EventKind::RuntimeStart.to_string(), "runtime_start");
        assert_eq!(EventKind::SessionStart.to_string(), "session_start");
        assert_eq!(EventKind::TuiReady.to_string(), "tui_ready");
        assert_eq!(EventKind::AgentStart.to_string(), "agent_start");
        assert_eq!(EventKind::AgentEnd.to_string(), "agent_end");
        assert_eq!(EventKind::TurnStart.to_string(), "turn_start");
        assert_eq!(EventKind::TurnEnd.to_string(), "turn_end");
        assert_eq!(
            EventKind::BeforeAgentStart.to_string(),
            "before_agent_start"
        );
        assert_eq!(EventKind::Context.to_string(), "context");
        assert_eq!(EventKind::ToolCall.to_string(), "tool_call");
        assert_eq!(EventKind::ToolResult.to_string(), "tool_result");
        assert_eq!(EventKind::Input.to_string(), "input");
        assert_eq!(EventKind::SessionShutdown.to_string(), "session_shutdown");
    }

    #[test]
    fn test_event_kind_serialization() {
        assert_eq!(
            serde_json::to_value(EventKind::RuntimeStart).unwrap(),
            json!("RuntimeStart")
        );
        assert_eq!(
            serde_json::to_value(EventKind::SessionStart).unwrap(),
            json!("SessionStart")
        );
        assert_eq!(
            serde_json::to_value(EventKind::BeforeAgentStart).unwrap(),
            json!("BeforeAgentStart")
        );
        assert_eq!(
            serde_json::to_value(EventKind::SessionShutdown).unwrap(),
            json!("SessionShutdown")
        );
    }

    #[test]
    fn test_event_kind_deserialization() {
        let kind: EventKind = serde_json::from_value(json!("RuntimeStart")).unwrap();
        assert_eq!(kind, EventKind::RuntimeStart);

        let kind: EventKind = serde_json::from_value(json!("ToolCall")).unwrap();
        assert_eq!(kind, EventKind::ToolCall);

        let kind: EventKind = serde_json::from_value(json!("Input")).unwrap();
        assert_eq!(kind, EventKind::Input);
    }

    #[test]
    fn test_event_kind_round_trip() {
        for kind in [
            EventKind::RuntimeStart,
            EventKind::SessionStart,
            EventKind::TuiReady,
            EventKind::AgentStart,
            EventKind::AgentEnd,
            EventKind::TurnStart,
            EventKind::TurnEnd,
            EventKind::BeforeAgentStart,
            EventKind::Context,
            EventKind::ToolCall,
            EventKind::ToolResult,
            EventKind::Input,
            EventKind::SessionShutdown,
        ] {
            let json = serde_json::to_value(kind).unwrap();
            let deserialized: EventKind = serde_json::from_value(json).unwrap();
            assert_eq!(kind, deserialized);
        }
    }

    // --- EventPayload tests ---

    #[test]
    fn test_event_payload_empty() {
        let payload = EventPayload::Empty;
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json, json!("Empty"));
        let deserialized: EventPayload = serde_json::from_value(json).unwrap();
        assert!(matches!(deserialized, EventPayload::Empty));
    }

    #[test]
    fn test_event_payload_session_start() {
        let payload = EventPayload::SessionStart {
            cwd: "/home/user".to_string(),
            session_id: "sess-123".to_string(),
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["SessionStart"]["cwd"], "/home/user");
        assert_eq!(json["SessionStart"]["session_id"], "sess-123");

        let deserialized: EventPayload = serde_json::from_value(json).unwrap();
        match deserialized {
            EventPayload::SessionStart { cwd, session_id } => {
                assert_eq!(cwd, "/home/user");
                assert_eq!(session_id, "sess-123");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_event_payload_before_agent_start() {
        let payload = EventPayload::BeforeAgentStart {
            prompt: "hello".to_string(),
            system_prompt: "system".to_string(),
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["BeforeAgentStart"]["prompt"], "hello");
        assert_eq!(json["BeforeAgentStart"]["system_prompt"], "system");

        let deserialized: EventPayload = serde_json::from_value(json).unwrap();
        match deserialized {
            EventPayload::BeforeAgentStart {
                prompt,
                system_prompt,
            } => {
                assert_eq!(prompt, "hello");
                assert_eq!(system_prompt, "system");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_event_payload_context() {
        let payload = EventPayload::Context { turn: 5 };
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["Context"]["turn"], 5);

        let deserialized: EventPayload = serde_json::from_value(json).unwrap();
        assert!(matches!(deserialized, EventPayload::Context { turn: 5 }));
    }

    #[test]
    fn test_event_payload_turn_start() {
        let payload = EventPayload::TurnStart { turn: 3 };
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["TurnStart"]["turn"], 3);

        let deserialized: EventPayload = serde_json::from_value(json).unwrap();
        assert!(matches!(deserialized, EventPayload::TurnStart { turn: 3 }));
    }

    #[test]
    fn test_event_payload_turn_end() {
        let payload = EventPayload::TurnEnd {
            turn: 7,
            had_code: true,
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["TurnEnd"]["turn"], 7);
        assert_eq!(json["TurnEnd"]["had_code"], true);

        let deserialized: EventPayload = serde_json::from_value(json).unwrap();
        assert!(matches!(
            deserialized,
            EventPayload::TurnEnd {
                turn: 7,
                had_code: true
            }
        ));
    }

    #[test]
    fn test_event_payload_agent_end() {
        let payload = EventPayload::AgentEnd {
            stop_cause: "done".to_string(),
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["AgentEnd"]["stop_cause"], "done");

        let deserialized: EventPayload = serde_json::from_value(json).unwrap();
        match deserialized {
            EventPayload::AgentEnd { stop_cause } => {
                assert_eq!(stop_cause, "done");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_event_payload_tool_call() {
        let payload = EventPayload::ToolCall {
            name: "bash".to_string(),
            args: json!({"cmd": "ls"}),
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["ToolCall"]["name"], "bash");
        assert_eq!(json["ToolCall"]["args"]["cmd"], "ls");

        let deserialized: EventPayload = serde_json::from_value(json).unwrap();
        match deserialized {
            EventPayload::ToolCall { name, args } => {
                assert_eq!(name, "bash");
                assert_eq!(args["cmd"], "ls");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_event_payload_tool_result() {
        let payload = EventPayload::ToolResult {
            name: "bash".to_string(),
            result: "output".to_string(),
            error: Some("oops".to_string()),
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["ToolResult"]["name"], "bash");
        assert_eq!(json["ToolResult"]["result"], "output");
        assert_eq!(json["ToolResult"]["error"], "oops");

        let deserialized: EventPayload = serde_json::from_value(json).unwrap();
        match deserialized {
            EventPayload::ToolResult {
                name,
                result,
                error,
            } => {
                assert_eq!(name, "bash");
                assert_eq!(result, "output");
                assert_eq!(error, Some("oops".to_string()));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_event_payload_tool_result_no_error() {
        let payload = EventPayload::ToolResult {
            name: "bash".to_string(),
            result: "output".to_string(),
            error: None,
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert!(json["ToolResult"]["error"].is_null());

        let deserialized: EventPayload = serde_json::from_value(json).unwrap();
        match deserialized {
            EventPayload::ToolResult { error, .. } => {
                assert_eq!(error, None);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_event_payload_input() {
        let payload = EventPayload::Input {
            text: "hello world".to_string(),
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["Input"]["text"], "hello world");

        let deserialized: EventPayload = serde_json::from_value(json).unwrap();
        match deserialized {
            EventPayload::Input { text } => {
                assert_eq!(text, "hello world");
            }
            _ => panic!("wrong variant"),
        }
    }

    // --- LifecycleEvent tests ---

    #[test]
    fn test_lifecycle_event_serialization() {
        let event = LifecycleEvent {
            kind: EventKind::TurnStart,
            payload: EventPayload::TurnStart { turn: 1 },
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["kind"], "TurnStart");
        assert_eq!(json["payload"]["TurnStart"]["turn"], 1);
    }

    #[test]
    fn test_lifecycle_event_round_trip() {
        let event = LifecycleEvent {
            kind: EventKind::Context,
            payload: EventPayload::Context { turn: 42 },
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: LifecycleEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event.kind, deserialized.kind);
        assert!(matches!(
            deserialized.payload,
            EventPayload::Context { turn: 42 }
        ));
    }

    // --- InputActionKind tests ---

    #[test]
    fn test_input_action_kind_serialization() {
        assert_eq!(
            serde_json::to_value(InputActionKind::Continue).unwrap(),
            json!("Continue")
        );
        assert_eq!(
            serde_json::to_value(InputActionKind::Transform).unwrap(),
            json!("Transform")
        );
        assert_eq!(
            serde_json::to_value(InputActionKind::Handled).unwrap(),
            json!("Handled")
        );
    }

    #[test]
    fn test_input_action_kind_deserialization() {
        let kind: InputActionKind = serde_json::from_value(json!("Continue")).unwrap();
        assert!(matches!(kind, InputActionKind::Continue));

        let kind: InputActionKind = serde_json::from_value(json!("Transform")).unwrap();
        assert!(matches!(kind, InputActionKind::Transform));

        let kind: InputActionKind = serde_json::from_value(json!("Handled")).unwrap();
        assert!(matches!(kind, InputActionKind::Handled));
    }

    // --- EventHandlerRegistration (Deserialize with skip on handler) ---

    #[test]
    fn test_event_handler_registration_deserialization() {
        let json = json!({
            "event": "RuntimeStart",
            "label": "my-handler"
        });
        let reg: EventHandlerRegistration = serde_json::from_value(json).unwrap();
        assert_eq!(reg.event, EventKind::RuntimeStart);
        assert_eq!(reg.label, "my-handler");
    }
}
