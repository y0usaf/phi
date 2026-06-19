pub mod command;
pub mod event;
pub mod manifest;
pub mod tool;
pub mod traits;

pub use command::{CommandHandler, CommandInvocation, CommandOutput, CommandSpec};
pub use event::{
    EventHandler, EventHandlerRegistration, EventKind, EventPayload, EventReturn, InputActionKind,
    LifecycleEvent,
};
pub use manifest::ExtensionManifest;
pub use tool::{ToolHandler, ToolInvocation, ToolOutput, ToolParam, ToolSpec};
pub use traits::{Extension, ExtensionHost};


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // --- Manifest tests ---

    #[test]
    fn test_extension_manifest_creation() {
        let manifest = ExtensionManifest {
            id: "test-ext".to_string(),
            name: "Test Extension".to_string(),
            version: "1.0.0".to_string(),
            description: "A test extension".to_string(),
        };
        assert_eq!(manifest.id, "test-ext");
        assert_eq!(manifest.name, "Test Extension");
        assert_eq!(manifest.version, "1.0.0");
        assert_eq!(manifest.description, "A test extension");
    }

    #[test]
    fn test_extension_manifest_serialization() {
        let manifest = ExtensionManifest {
            id: "test-ext".to_string(),
            name: "Test Extension".to_string(),
            version: "1.0.0".to_string(),
            description: "A test extension".to_string(),
        };
        let json = serde_json::to_value(&manifest).unwrap();
        assert_eq!(json["id"], "test-ext");
        assert_eq!(json["name"], "Test Extension");
        assert_eq!(json["version"], "1.0.0");
        assert_eq!(json["description"], "A test extension");
    }

    #[test]
    fn test_extension_manifest_deserialization() {
        let json = json!({
            "id": "test-ext",
            "name": "Test Extension",
            "version": "1.0.0",
            "description": "A test extension"
        });
        let manifest: ExtensionManifest = serde_json::from_value(json).unwrap();
        assert_eq!(manifest.id, "test-ext");
        assert_eq!(manifest.name, "Test Extension");
        assert_eq!(manifest.version, "1.0.0");
        assert_eq!(manifest.description, "A test extension");
    }

    #[test]
    fn test_extension_manifest_round_trip() {
        let manifest = ExtensionManifest {
            id: "round-trip".to_string(),
            name: "Round Trip".to_string(),
            version: "0.2.0".to_string(),
            description: "Round trip test".to_string(),
        };
        let json = serde_json::to_string(&manifest).unwrap();
        let deserialized: ExtensionManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(manifest.id, deserialized.id);
        assert_eq!(manifest.name, deserialized.name);
        assert_eq!(manifest.version, deserialized.version);
        assert_eq!(manifest.description, deserialized.description);
    }

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
        assert_eq!(EventKind::BeforeAgentStart.to_string(), "before_agent_start");
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
        assert!(matches!(deserialized, EventPayload::TurnEnd { turn: 7, had_code: true }));
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

    // --- CommandInvocation / CommandOutput tests ---

    #[test]
    fn test_command_invocation_serialization() {
        let inv = CommandInvocation {
            raw_args: "arg1 arg2".to_string(),
        };
        let json = serde_json::to_value(&inv).unwrap();
        assert_eq!(json["raw_args"], "arg1 arg2");
    }

    #[test]
    fn test_command_invocation_round_trip() {
        let inv = CommandInvocation {
            raw_args: "test args".to_string(),
        };
        let json = serde_json::to_string(&inv).unwrap();
        let deserialized: CommandInvocation = serde_json::from_str(&json).unwrap();
        assert_eq!(inv.raw_args, deserialized.raw_args);
    }

    #[test]
    fn test_command_output_with_message() {
        let out = CommandOutput {
            message: Some("hello".to_string()),
        };
        let json = serde_json::to_value(&out).unwrap();
        assert_eq!(json["message"], "hello");

        let deserialized: CommandOutput = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized.message, Some("hello".to_string()));
    }

    #[test]
    fn test_command_output_no_message() {
        let out = CommandOutput {
            message: None,
        };
        let json = serde_json::to_value(&out).unwrap();
        assert!(json["message"].is_null());

        let deserialized: CommandOutput = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized.message, None);
    }

    // --- CommandSpec serialization (no Deserialize) ---

    #[test]
    fn test_command_spec_serialization() {
        let spec = CommandSpec {
            name: "test-cmd".to_string(),
            description: "A test command".to_string(),
            handler: std::sync::Arc::new(|_inv| Ok(CommandOutput { message: None })),
        };
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json["name"], "test-cmd");
        assert_eq!(json["description"], "A test command");
        // handler should be skipped
        assert!(json.get("handler").is_none());
        assert_eq!(serde_json::to_string(&spec).unwrap().contains("handler"), false);
    }

    // --- ToolParam tests ---

    #[test]
    fn test_tool_param_with_default() {
        let param = ToolParam {
            name: "path".to_string(),
            ty: "string".to_string(),
            default: Some("/tmp".to_string()),
        };
        let json = serde_json::to_value(&param).unwrap();
        assert_eq!(json["name"], "path");
        assert_eq!(json["ty"], "string");
        assert_eq!(json["default"], "/tmp");

        let deserialized: ToolParam = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized.name, "path");
        assert_eq!(deserialized.ty, "string");
        assert_eq!(deserialized.default, Some("/tmp".to_string()));
    }

    #[test]
    fn test_tool_param_no_default() {
        let param = ToolParam {
            name: "count".to_string(),
            ty: "number".to_string(),
            default: None,
        };
        let json = serde_json::to_value(&param).unwrap();
        assert!(json["default"].is_null());

        let deserialized: ToolParam = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized.default, None);
    }

    // --- ToolInvocation / ToolOutput tests ---

    #[test]
    fn test_tool_invocation_round_trip() {
        let inv = ToolInvocation {
            input: "test input".to_string(),
        };
        let json = serde_json::to_string(&inv).unwrap();
        let deserialized: ToolInvocation = serde_json::from_str(&json).unwrap();
        assert_eq!(inv.input, deserialized.input);
    }

    #[test]
    fn test_tool_output_round_trip() {
        let out = ToolOutput {
            content: "result content".to_string(),
        };
        let json = serde_json::to_string(&out).unwrap();
        let deserialized: ToolOutput = serde_json::from_str(&json).unwrap();
        assert_eq!(out.content, deserialized.content);
    }

    // --- ToolSpec serialization (no Deserialize) ---

    #[test]
    fn test_tool_spec_serialization() {
        let param = ToolParam {
            name: "x".to_string(),
            ty: "string".to_string(),
            default: None,
        };
        let spec = ToolSpec {
            name: "test-tool".to_string(),
            description: "A test tool".to_string(),
            params: vec![param],
            handler: std::sync::Arc::new(|_inv| {
                Ok(ToolOutput {
                    content: "ok".to_string(),
                })
            }),
        };
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json["name"], "test-tool");
        assert_eq!(json["description"], "A test tool");
        assert_eq!(json["params"][0]["name"], "x");
        assert_eq!(json["params"][0]["ty"], "string");
        // handler should be skipped
        assert!(json.get("handler").is_none());
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