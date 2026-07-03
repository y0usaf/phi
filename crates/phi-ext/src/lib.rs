pub mod animation_spec;
pub mod builder;
pub mod command;
pub mod draw_context;
pub mod event;
pub mod flag;
pub mod host;
pub mod manifest;
pub mod message_renderer;
pub mod overlay_spec;
pub mod provider;
pub mod runtime;
pub mod shortcut;
pub mod snapshot;
pub mod theme_spec;
pub mod tool;
pub mod tool_render;
pub mod traits;
pub mod visual;

pub use builder::RuntimeBuilder;
pub use command::{
    ArgCompletionFn, CommandContext, CommandHandler, CommandInfo, CommandInvocation, CommandOutput,
    CommandSpec, PickerKind, UiAction,
};
pub use draw_context::{
    DrawContext, DrawRect, DrawSize, OverlayKey, OverlayKeyCode, OverlayKeyModifiers,
    OverlayPalette, brighten,
};
pub use event::{
    EventHandler, EventHandlerRegistration, EventKind, EventPayload, EventReturn, InputActionKind,
    LifecycleEvent,
};
pub use flag::{FlagSpec, FlagType};
pub use manifest::ExtensionManifest;
pub use message_renderer::MessageRendererSpec;
pub use overlay_spec::{
    HelpEntry, HelpPayload, ModelPickerPayload, OVERLAY_ARCHIVE, OVERLAY_HELP, OVERLAY_MODEL,
    OVERLAY_USAGE, OverlayInitFn, OverlayKeyFn, OverlayOutcome, OverlayPayload, OverlayRenderFn,
    OverlayResponder, OverlayResponse, OverlayScrollFn, OverlaySpec, OverlayState,
};
pub use provider::{ProviderConfig, ProviderModelConfig, ProviderModelCost};
pub use runtime::{
    AppRuntime, DiscoveredResources, ForkDecision, ProjectTrustDecision, ToolInterception,
    TreeDecision, missing_named_item,
};
pub use shortcut::ShortcutSpec;
pub use snapshot::{
    CommandSummary, EventHandlerSummary, FlagSummary, OverlaySummary, RuntimeSnapshot,
    ShortcutSummary, SpinnerSummary, TextEffectSummary, ThemeSummary, ThinkingExpressionSummary,
    ToolSummary,
};
pub use theme_spec::{TerminalColorsProbe, ThemeSpec};
pub use tool::{
    ToolDispatch, ToolExposure, ToolHandler, ToolInvocation, ToolOutput, ToolParam, ToolSpec,
    python_param_name, python_param_names, schema_from_params,
};
pub use tool_render::{
    AnchorArg, AnchorStyle, ResultLang, ToolBody, ToolRenderCategory, ToolRenderIndex,
    ToolRenderSpec, repl_render_spec, tally, title_case_tool_name,
};
pub use traits::{AnimationSpec, Extension, ExtensionHost};
pub use visual::{Rgb, StyledSpan, StyledText};

// Re-export animation spec types
pub use animation_spec::{SpinnerSpec, TextEffectSpec, ThinkingExpressionSpec};

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
            args_hint: String::new(),
            handler: std::sync::Arc::new(|_inv| Ok(CommandOutput::default())),
            arg_completions: None,
            context: CommandContext::Session,
            aliases: Vec::new(),
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
        let spec = ToolSpec::repl(
            "test-tool",
            "A test tool",
            vec![param],
            std::sync::Arc::new(|_inv| {
                Ok(ToolOutput {
                    content: "ok".to_string(),
                })
            }),
        );
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json["name"], "test-tool");
        assert_eq!(json["description"], "A test tool");
        assert_eq!(json["params"][0]["name"], "x");
        assert_eq!(json["expose"], "repl");
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
                cost: None,
                thinking_levels: None,
                input: None,
                headers: Default::default(),
                adaptive_thinking: false,
            }],
        };
        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(json["name"], "my-proxy");
        assert_eq!(json["base_url"], "https://proxy.example.com");
        assert_eq!(json["models"][0]["id"], "model-1");
        // The fidelity fields are optional in both directions: a minimal
        // hand-written config (no cost/thinking/modalities) round-trips.
        let minimal: ProviderModelConfig =
            serde_json::from_value(json!({"id": "m", "name": "M"})).unwrap();
        assert!(minimal.cost.is_none());
        assert!(minimal.thinking_levels.is_none());
        assert!(minimal.headers.is_empty());
    }

    // --- Visual spec tests ---

    #[test]
    fn test_rgb_serialization() {
        let rgb = Rgb(255, 128, 0);
        let json = serde_json::to_value(&rgb).unwrap();
        assert_eq!(json, json!([255, 128, 0]));
    }

    #[test]
    fn test_styled_text_to_ansi() {
        let mut st = StyledText::new();
        st.push_span(StyledSpan::new("hello").with_fg(Rgb(255, 0, 0)).bold());
        st.push_span(StyledSpan::new(" world"));
        let ansi = st.to_ansi();
        assert!(ansi.contains("\x1b["));
        assert!(ansi.contains("hello"));
        assert!(ansi.contains(" world"));
    }

    #[test]
    fn test_spinner_spec_frame_index() {
        let spinner = SpinnerSpec::new("test", vec!["a".into(), "b".into(), "c".into()], 100);
        assert_eq!(spinner.frame_at(0), "a");
        assert_eq!(spinner.frame_at(100), "b");
        assert_eq!(spinner.frame_at(200), "c");
        assert_eq!(spinner.frame_at(300), "a"); // wraps
    }

    #[test]
    fn test_thinking_expression_spec() {
        let expr = ThinkingExpressionSpec::new(
            "test",
            vec!["thinking...".into(), "pondering...".into()],
            "working...",
            2000,
        );
        assert_eq!(expr.expression(0, false), "thinking...");
        assert_eq!(expr.expression(2000, false), "pondering...");
        assert_eq!(expr.expression(0, true), "working...");
    }

    #[test]
    fn test_theme_spec_declarative() {
        let mut palette = std::collections::BTreeMap::new();
        palette.insert("foreground".into(), Rgb(255, 255, 255));
        palette.insert("background".into(), Rgb(0, 0, 0));
        let theme = ThemeSpec::new("dark", palette);
        let resolved = theme.resolve(None);
        assert_eq!(resolved.get("foreground"), Some(&Rgb(255, 255, 255)));
        assert_eq!(resolved.get("background"), Some(&Rgb(0, 0, 0)));
    }

    // --- Overlay spec tests ---

    #[test]
    fn test_overlay_spec_creation() {
        use std::any::Any;
        use std::sync::Arc;

        let overlay = OverlaySpec::new(
            "test-overlay",
            "Test overlay",
            Arc::new(|_payload| Box::new(0i32) as Box<dyn Any + Send + Sync>),
            Arc::new(
                |_ctx: &mut dyn crate::DrawContext, _state: &mut Box<dyn Any + Send + Sync>| {},
            ),
            Arc::new(
                |_state: &mut Box<dyn Any + Send + Sync>,
                 _key: &crate::OverlayKey,
                 _c: u16,
                 _r: u16| { crate::OverlayOutcome::None },
            ),
        )
        .with_toggle_key("t");

        assert_eq!(overlay.name, "test-overlay");
        assert_eq!(overlay.description, "Test overlay");
        assert_eq!(overlay.toggle_key.as_deref(), Some("t"));
    }

    #[test]
    fn test_overlay_in_snapshot() {
        use std::any::Any;
        use std::sync::Arc;

        let builder = RuntimeBuilder::new().with_cwd(std::path::Path::new("."));

        struct OverlayExt;
        impl Extension for OverlayExt {
            fn manifest(&self) -> ExtensionManifest {
                ExtensionManifest {
                    id: "test.overlay".to_string(),
                    name: "Overlay".to_string(),
                    version: "0".to_string(),
                    description: "test".to_string(),
                }
            }
            fn register(&self, host: &mut dyn ExtensionHost) -> anyhow::Result<()> {
                host.register_overlay(
                    OverlaySpec::new(
                        "my-overlay",
                        "desc",
                        Arc::new(|_payload| Box::new(()) as Box<dyn Any + Send + Sync>),
                        Arc::new(|_, _| {}),
                        Arc::new(|_, _, _, _| crate::OverlayOutcome::None),
                    )
                    .with_toggle_key("o"),
                )?;
                Ok(())
            }
        }

        let rt = builder.register_extension(OverlayExt).build().unwrap();
        assert_eq!(rt.list_overlays(), vec!["my-overlay"]);
        assert_eq!(rt.get_overlay("my-overlay").unwrap().description, "desc");

        let snap = rt.snapshot();
        assert_eq!(snap.overlays.len(), 1);
        assert_eq!(snap.overlays[0].name, "my-overlay");
        assert_eq!(snap.overlays[0].toggle_key.as_deref(), Some("o"));
    }
}
