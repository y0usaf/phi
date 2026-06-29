pub mod builder;
pub mod command;
pub mod event;
pub mod flag;
pub mod host;
pub mod manifest;
pub mod message_renderer;
pub mod provider;
pub mod runtime;
pub mod shortcut;
pub mod snapshot;
pub mod tool;
pub mod traits;

pub use builder::RuntimeBuilder;
pub use command::{CommandHandler, CommandInvocation, CommandOutput, CommandSpec};
pub use event::{
    EventHandler, EventHandlerRegistration, EventKind, EventPayload, EventReturn, InputActionKind,
    LifecycleEvent,
};
pub use flag::{FlagSpec, FlagType};
pub use manifest::ExtensionManifest;
pub use message_renderer::MessageRendererSpec;
pub use provider::{ProviderConfig, ProviderModelConfig};
pub use runtime::{AppRuntime, DiscoveredResources, ProjectTrustDecision, missing_named_item};
pub use shortcut::ShortcutSpec;
pub use snapshot::{CommandSummary, EventHandlerSummary, FlagSummary, RuntimeSnapshot, ShortcutSummary, ToolSummary};
pub use tool::{ToolHandler, ToolInvocation, ToolOutput, ToolParam, ToolSpec};
pub use traits::{Extension, ExtensionHost};

// Re-export extension context types from phi-event
pub use phi_event::{ExtensionContextRef, ExtensionMode};

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

    // --- CommandSpec serialization ---

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
        assert!(json.get("handler").is_none());
    }

    // --- ToolSpec serialization ---

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
    }

    // --- FlagSpec tests ---

    #[test]
    fn test_flag_spec_creation() {
        let flag = FlagSpec {
            name: "verbose".to_string(),
            description: "Enable verbose output".to_string(),
            flag_type: FlagType::Boolean,
            default: Some("false".to_string()),
        };
        assert_eq!(flag.name, "verbose");
        assert_eq!(flag.flag_type, FlagType::Boolean);
    }

    // --- ProviderConfig tests ---

    #[test]
    fn test_provider_config_serialization() {
        let config = ProviderConfig {
            name: "my-proxy".to_string(),
            base_url: Some("https://proxy.example.com".to_string()),
            api_key: Some("key".to_string()),
            api: Some("anthropic-messages".to_string()),
            models: vec![ProviderModelConfig {
                id: "model-1".to_string(),
                name: "Model One".to_string(),
                reasoning: false,
                context_window: 200000,
                max_tokens: 16384,
            }],
        };
        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(json["name"], "my-proxy");
        assert_eq!(json["base_url"], "https://proxy.example.com");
        assert_eq!(json["models"][0]["id"], "model-1");
    }
}
