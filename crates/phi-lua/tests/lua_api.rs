//! Integration tests for the expanded Lua extension bridge.
//! Verifies that Lua scripts can register tools, shortcuts, flags, and providers
//! through the `phi.*` API surface.

use std::sync::Arc;
use phi_ext::{RuntimeBuilder, EventKind, EventReturn, LifecycleEvent};
use phi_lua::LuaExtension;

/// Helper: write a Lua source to a temp file and load it as a LuaExtension.
fn load_lua(source: &str) -> Arc<phi_ext::AppRuntime> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test_ext.lua");
    std::fs::write(&path, source).unwrap();
    let ext = LuaExtension::from_file(&path).unwrap();

    // We need to keep the temp dir alive — leak it for the test duration.
    std::mem::forget(dir);

    let rt = RuntimeBuilder::new()
        .with_cwd(std::path::Path::new("."))
        .register_extension(ext)
        .build()
        .unwrap();
    Arc::new(rt)
}

#[tokio::test]
async fn lua_register_tool_works() {
    let source = r#"
phi.register_tool("greet", {
    description = "Greet someone",
    params = {},
    handler = function(input)
        return { content = "hello from lua tool" }
    end
})
"#;
    let rt = load_lua(source);

    // The tool should be registered and callable
    let result = rt.invoke_tool_sync("greet", "").unwrap();
    assert!(result.is_some());
    let output = result.unwrap();
    assert_eq!(output.content, "hello from lua tool");
}

#[tokio::test]
async fn lua_register_command_works() {
    let source = r#"
phi.register_command("say-hello", {
    description = "Say hello",
    handler = function(args)
        return { message = "hello " .. args }
    end
})
"#;
    let rt = load_lua(source);

    let output = rt.invoke_command("say-hello", "world").await.unwrap();
    assert!(output.is_some());
    assert_eq!(output.unwrap().message, Some("hello world".to_string()));
}

#[tokio::test]
async fn lua_on_event_works() {
    let source = r#"
phi.on("turn_start", function(payload)
    return {
        system_prompt_append = "Lua hook was here"
    }
end)
"#;
    let rt = load_lua(source);

    // Dispatch a before_agent_start event (uses system_prompt_append return)
    // Actually, the hook is on turn_start, but system_prompt_append only works
    // for before_agent_start. Let's test it dispatches without error.
    let returns = rt
        .dispatch(LifecycleEvent {
            kind: EventKind::TurnStart,
            payload: phi_ext::EventPayload::TurnStart { turn: 1 },
        })
        .await
        .unwrap();
    assert!(!returns.is_empty());
    // The return should be decoded as SystemPromptMod (even though it's
    // semantically wrong for turn_start, the decode doesn't check event kind)
    match &returns[0] {
        EventReturn::SystemPromptMod { append, .. } => {
            assert_eq!(append.as_deref(), Some("Lua hook was here"));
        }
        other => panic!("expected SystemPromptMod, got {other:?}"),
    }
}

#[tokio::test]
async fn lua_register_flag_works() {
    let source = r#"
phi.register_flag("verbose", {
    description = "Enable verbose output",
    type = "boolean",
    default = "false"
})
"#;
    let rt = load_lua(source);

    let flag = rt.get_flag("verbose");
    assert!(flag.is_some());
    let flag = flag.unwrap();
    assert_eq!(flag.name, "verbose");
    assert_eq!(flag.description, "Enable verbose output");
    assert_eq!(flag.flag_type, phi_ext::FlagType::Boolean);
}

#[tokio::test]
async fn lua_register_shortcut_works() {
    let source = r#"
phi.register_shortcut("C-c", {
    description = "Copy to clipboard",
    handler = function() end
})
"#;
    let rt = load_lua(source);

    let shortcuts = rt.list_shortcuts();
    assert_eq!(shortcuts.len(), 1);
    assert_eq!(shortcuts[0].0, "C-c");
    assert_eq!(shortcuts[0].1, "Copy to clipboard");
}

#[tokio::test]
async fn lua_register_provider_works() {
    let source = r#"
phi.register_provider("my-proxy", {
    base_url = "https://proxy.example.com",
    api_key = "secret-key",
    api = "anthropic-messages",
    models = {
        {
            id = "model-1",
            name = "Model One",
            reasoning = false,
            context_window = 200000,
            max_tokens = 16384
        }
    }
})
"#;
    let rt = load_lua(source);

    let provider = rt.get_provider("my-proxy");
    assert!(provider.is_some());
    let p = provider.unwrap();
    assert_eq!(p.base_url.as_deref(), Some("https://proxy.example.com"));
    assert_eq!(p.api.as_deref(), Some("anthropic-messages"));
    assert_eq!(p.models.len(), 1);
    assert_eq!(p.models[0].id, "model-1");
    assert_eq!(p.models[0].context_window, 200000);
}

#[tokio::test]
async fn lua_multiple_registrations() {
    let source = r#"
-- Register multiple things in one script
phi.register_tool("tool1", {
    description = "First tool",
    handler = function(input) return { content = "tool1 result" } end
})
phi.register_tool("tool2", {
    description = "Second tool",
    handler = function(input) return { content = "tool2 result" } end
})
phi.register_command("cmd1", {
    description = "First command",
    handler = function(args) return { message = "cmd1: " .. args } end
})
phi.register_flag("debug", {
    description = "Debug mode",
    type = "boolean",
    default = "false"
})
"#;
    let rt = load_lua(source);

    assert_eq!(rt.all_tool_names().len(), 2);
    assert!(rt.all_tool_names().contains(&"tool1".to_string()));
    assert!(rt.all_tool_names().contains(&"tool2".to_string()));

    let out1 = rt.invoke_tool_sync("tool1", "").unwrap().unwrap();
    assert_eq!(out1.content, "tool1 result");

    let out2 = rt.invoke_tool_sync("tool2", "").unwrap().unwrap();
    assert_eq!(out2.content, "tool2 result");

    let cmd = rt.invoke_command("cmd1", "test").await.unwrap().unwrap();
    assert_eq!(cmd.message, Some("cmd1: test".to_string()));

    assert!(rt.get_flag("debug").is_some());
}

#[tokio::test]
async fn lua_cancel_event_works() {
    let source = r#"
phi.on("session_before_switch", function(payload)
    return { cancel = true, reason = "blocked by lua" }
end)
"#;
    let rt = load_lua(source);

    let cancelled = rt
        .before_session_switch("new", None)
        .await
        .unwrap();
    assert!(cancelled);
}

#[tokio::test]
async fn lua_resources_discover_works() {
    let source = r#"
phi.on("resources_discover", function(payload)
    return {
        skill_paths = { "/custom/skills" },
        prompt_paths = { "/custom/prompts" },
        theme_paths = {}
    }
end)
"#;
    let rt = load_lua(source);

    let resources = rt
        .discover_resources(".", "startup")
        .await
        .unwrap();
    assert_eq!(resources.skill_paths, vec!["/custom/skills"]);
    assert_eq!(resources.prompt_paths, vec!["/custom/prompts"]);
}
