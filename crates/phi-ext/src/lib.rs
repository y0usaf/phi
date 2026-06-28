pub mod builder;
pub mod command;
pub mod event;
pub mod host;
pub mod manifest;
pub mod runtime;
pub mod snapshot;
pub mod tool;
pub mod traits;

pub use builder::RuntimeBuilder;
pub use command::{CommandHandler, CommandInvocation, CommandOutput, CommandSpec};
pub use event::{
    EventHandler, EventHandlerRegistration, EventKind, EventPayload, EventReturn, InputActionKind,
    LifecycleEvent,
};
pub use manifest::ExtensionManifest;
pub use runtime::{AppRuntime, missing_named_item};
pub use snapshot::{CommandSummary, EventHandlerSummary, RuntimeSnapshot, ToolSummary};
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
        let out = CommandOutput { message: None };
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
        assert!(!serde_json::to_string(&spec).unwrap().contains("handler"));
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
}
