//! Integration tests for the expanded Lua extension bridge.
//! Verifies that Lua scripts can register tools, shortcuts, flags, and providers
//! through the `phi.*` API surface.

use phi_ext::{EventKind, EventReturn, LifecycleEvent, RuntimeBuilder};
use phi_lua::LuaExtension;
use std::sync::Arc;

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

    let cancelled = rt.before_session_switch("new", None).await.unwrap();
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

    let resources = rt.discover_resources(".", "startup").await.unwrap();
    assert_eq!(resources.skill_paths, vec!["/custom/skills"]);
    assert_eq!(resources.prompt_paths, vec!["/custom/prompts"]);
}

/// A Lua handler that blocks in a **native** call (`os.execute` of a sleeping
/// subprocess) cannot be interrupted by the instruction-counting watchdog (the
/// hook only fires between Lua bytecode instructions). This is the real-world
/// freeze vector. The central dispatch timeout (Fix 1) must bound it so the
/// caller stays responsive even while the Lua OS thread is wedged.
#[cfg(unix)]
#[tokio::test]
async fn lua_native_blocked_handler_is_bounded() {
    use std::time::{Duration, Instant};

    // `turn_start` is a 250ms notification. The handler sleeps 2s in a native
    // call — far past the budget and past the watchdog's reach.
    let source = r#"
phi.on("turn_start", function(payload)
    os.execute("sleep 2")
    return nil
end)
"#;
    let rt = load_lua(source);

    let start = Instant::now();
    let returns = rt
        .dispatch(LifecycleEvent {
            kind: EventKind::TurnStart,
            payload: phi_ext::EventPayload::TurnStart { turn: 1 },
        })
        .await
        .expect("dispatch");
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(1),
        "dispatch took {elapsed:?}; expected the ~250ms budget, not the 2s native block"
    );
    assert!(
        returns.is_empty(),
        "a timed-out handler must contribute no return"
    );
    let notes = rt.drain_notifications().join("\n");
    assert!(
        notes.contains("timed out"),
        "missing timeout notification: {notes}"
    );
}

#[tokio::test]
async fn lua_register_theme_works() {
    let source = r#"
phi.register_theme("my-theme", {
    foreground = {220, 220, 220},
    background = {20, 20, 26},
    accent = {100, 200, 220},
    border = {50, 50, 60},
    ansi_0 = {0, 0, 0},
    ansi_1 = {200, 80, 80}
})
"#;
    let rt = load_lua(source);

    let theme = rt.first_theme().unwrap();
    assert_eq!(theme.name, "my-theme");
    let resolved = theme.resolve(None);
    assert_eq!(
        resolved.get("foreground"),
        Some(&phi_ext::Rgb(220, 220, 220))
    );
    assert_eq!(resolved.get("background"), Some(&phi_ext::Rgb(20, 20, 26)));
    assert_eq!(resolved.get("accent"), Some(&phi_ext::Rgb(100, 200, 220)));
    assert_eq!(resolved.get("border"), Some(&phi_ext::Rgb(50, 50, 60)));
    assert_eq!(resolved.get("ansi_0"), Some(&phi_ext::Rgb(0, 0, 0)));
    assert_eq!(resolved.get("ansi_1"), Some(&phi_ext::Rgb(200, 80, 80)));
}

#[tokio::test]
async fn lua_register_spinner_works() {
    let source = r#"
phi.register_spinner("my-spinner", {
    frames = {"|", "/", "-", "\\"},
    frame_interval_ms = 120
})
"#;
    let rt = load_lua(source);

    let spinner = rt.first_spinner().unwrap();
    assert_eq!(spinner.name, "my-spinner");
    assert_eq!(spinner.frames.len(), 4);
    assert_eq!(spinner.frame_interval_ms, 120);
    assert_eq!(spinner.frame_at(0), "|");
    assert_eq!(spinner.frame_at(120), "/");
    assert_eq!(spinner.frame_at(240), "-");
    assert_eq!(spinner.frame_at(360), "\\");
    assert_eq!(spinner.frame_at(480), "|"); // wraps
}

#[tokio::test]
async fn lua_register_thinking_expression_works() {
    let source = r#"
phi.register_thinking_expression("my-expr", {
    expressions = {"thinking...", "pondering..."},
    tool_expression = "working...",
    expression_interval_ms = 1500
})
"#;
    let rt = load_lua(source);

    let expr = rt.first_thinking_expression().unwrap();
    assert_eq!(expr.name, "my-expr");
    assert_eq!(expr.expressions.len(), 2);
    assert_eq!(expr.tool_expression, "working...");
    assert_eq!(expr.expression_interval_ms, 1500);
    assert_eq!(expr.expression(0, false), "thinking...");
    assert_eq!(expr.expression(1500, false), "pondering...");
    assert_eq!(expr.expression(0, true), "working...");
}

#[tokio::test]
async fn lua_multiple_visual_registrations() {
    let source = r#"
phi.register_theme("theme-a", { foreground = {255, 255, 255} })
phi.register_theme("theme-b", { foreground = {0, 0, 0} })
phi.register_spinner("spinner-a", { frames = {"."}, frame_interval_ms = 100 })
phi.register_spinner("spinner-b", { frames = {"x", "y"}, frame_interval_ms = 200 })
phi.register_thinking_expression("expr-a", {
    expressions = {"a"},
    tool_expression = "b",
    expression_interval_ms = 1000
})
"#;
    let rt = load_lua(source);

    assert_eq!(rt.list_themes(), vec!["theme-a", "theme-b"]);
    assert_eq!(rt.list_spinners(), vec!["spinner-a", "spinner-b"]);
    assert_eq!(rt.list_thinking_expressions(), vec!["expr-a"]);

    let theme_a = rt.get_theme("theme-a").unwrap();
    let theme_b = rt.get_theme("theme-b").unwrap();
    assert_eq!(
        theme_a.resolve(None).get("foreground"),
        Some(&phi_ext::Rgb(255, 255, 255))
    );
    assert_eq!(
        theme_b.resolve(None).get("foreground"),
        Some(&phi_ext::Rgb(0, 0, 0))
    );

    let spinner_a = rt.get_spinner("spinner-a").unwrap();
    let spinner_b = rt.get_spinner("spinner-b").unwrap();
    assert_eq!(spinner_a.frames, vec!["."]);
    assert_eq!(spinner_b.frames, vec!["x", "y"]);
}

#[tokio::test]
async fn lua_register_overlay_works() {
    let source = r#"
phi.register_overlay("my-overlay", {
    description = "My custom overlay",
    toggle_key = "m",
    render = function()
        return {
            "This is a custom overlay",
            "registered from Lua!",
            "Press Esc to close.",
        }
    end
})
"#;
    let rt = load_lua(source);

    assert_eq!(rt.list_overlays(), vec!["my-overlay"]);
    let overlay = rt.get_overlay("my-overlay").unwrap();
    assert_eq!(overlay.name, "my-overlay");
    assert_eq!(overlay.description, "My custom overlay");
    assert_eq!(overlay.toggle_key, "m");
}
