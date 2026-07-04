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
async fn lua_agent_end_summary_reaches_extension() {
    // WS7.3 validation: the bounded subagent completion report crosses the
    // bridge — a Lua extension consumes `payload.summary` from agent_end.
    // The handler echoes the fields back so the assertion sees exactly what
    // Lua saw.
    let source = r#"
phi.on("agent_end", function(payload)
    local data = payload.data or payload
    return {
        system_prompt_append = data.stop_cause .. "|" .. (data.summary or "<none>")
    }
end)
"#;
    let rt = load_lua(source);

    let returns = rt
        .dispatch(LifecycleEvent {
            kind: EventKind::AgentEnd,
            payload: phi_ext::EventPayload::AgentEnd {
                stop_cause: "Answer".to_owned(),
                summary: Some("checked 3 files; no issues".to_owned()),
            },
        })
        .await
        .unwrap();
    match &returns[0] {
        EventReturn::SystemPromptMod { append, .. } => {
            assert_eq!(append.as_deref(), Some("Answer|checked 3 files; no issues"));
        }
        other => panic!("expected SystemPromptMod, got {other:?}"),
    }

    // Absent summary crosses as nil, not as an empty string.
    let returns = rt
        .dispatch(LifecycleEvent {
            kind: EventKind::AgentEnd,
            payload: phi_ext::EventPayload::AgentEnd {
                stop_cause: "Failed".to_owned(),
                summary: None,
            },
        })
        .await
        .unwrap();
    match &returns[0] {
        EventReturn::SystemPromptMod { append, .. } => {
            assert_eq!(append.as_deref(), Some("Failed|<none>"));
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
    assert_eq!(overlay.toggle_key.as_deref(), Some("m"));
}

// ───────────────────────── WS1.6 parity: commands ─────────────────────────

/// The WS1.6 dogfood: a Lua reimplementation of a builtin command that
/// returns `UiAction`s (`/usage` is `OpenOverlay` + message). Before the
/// bridge fix the actions were discarded (`actions: Vec::new()` hardcoded).
#[tokio::test]
async fn lua_command_returns_ui_actions() {
    let source = r#"
phi.register_command("usage2", {
    description = "Open the usage overlay (Lua port of /usage)",
    handler = function(args)
        return {
            message = "opening usage",
            actions = {
                { type = "OpenOverlay", data = { name = "phi:usage" } },
            },
        }
    end
})
phi.register_command("quit2", {
    description = "Quit (Lua port of /quit)",
    handler = function(args)
        return { actions = { { type = "Quit" } } }
    end
})
"#;
    let rt = load_lua(source);

    let output = rt.invoke_command("usage2", "").await.unwrap().unwrap();
    assert_eq!(output.message.as_deref(), Some("opening usage"));
    assert_eq!(
        output.actions,
        vec![phi_ext::UiAction::OpenOverlay {
            name: "phi:usage".to_owned(),
            payload: None,
        }]
    );

    let output = rt.invoke_command("quit2", "").await.unwrap().unwrap();
    assert_eq!(output.message, None);
    assert_eq!(output.actions, vec![phi_ext::UiAction::Quit]);
}

/// A malformed action is a hard, named error — not a silently dropped effect.
#[tokio::test]
async fn lua_command_bad_action_errors() {
    let source = r#"
phi.register_command("broken", {
    description = "Returns a bogus action",
    handler = function(args)
        return { actions = { { type = "NoSuchAction" } } }
    end
})
"#;
    let rt = load_lua(source);
    let err = rt.invoke_command("broken", "").await.unwrap_err();
    assert!(err.to_string().contains("NoSuchAction"), "{err}");
}

/// Context tagging, aliases, and static arg completions — the rest of the
/// `CommandSpec` surface builtins use (WS1.7 vocabulary).
#[tokio::test]
async fn lua_command_context_aliases_completions() {
    let source = r#"
phi.register_command("panes", {
    description = "A mux-context command",
    context = "mux",
    aliases = { "p" },
    handler = function(args) return { message = "mux:" .. args } end
})
phi.register_command("pick", {
    description = "Completes its first argument",
    completions = { "alpha", "beta" },
    handler = function(args) return { message = args } end
})
"#;
    let rt = load_lua(source);

    // Mux-context: invisible to the session command path, resolved
    // (including via alias) in the mux context.
    assert!(rt.invoke_command("panes", "").await.unwrap().is_none());
    let out = rt
        .invoke_command_sync_in(phi_ext::CommandContext::Mux, "panes", "x")
        .unwrap()
        .unwrap();
    assert_eq!(out.message.as_deref(), Some("mux:x"));
    let out = rt
        .invoke_command_sync_in(phi_ext::CommandContext::Mux, "p", "y")
        .unwrap()
        .unwrap();
    assert_eq!(out.message.as_deref(), Some("mux:y"));

    assert_eq!(
        rt.complete_command_arg("pick"),
        Some(vec!["alpha".to_owned(), "beta".to_owned()])
    );
}

// ───────────────────────── WS1.6 parity: overlays ─────────────────────────

/// Test double for `DrawContext`: records text writes so assertions can see
/// what a Lua overlay drew.
#[derive(Default)]
struct RecordingCtx {
    texts: Vec<(i32, i32, String, bool)>,
    boxes: Vec<phi_ext::DrawRect>,
    scrollbars: usize,
}

impl RecordingCtx {
    fn all_text(&self) -> String {
        self.texts
            .iter()
            .map(|(_, _, t, _)| t.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl phi_ext::DrawContext for RecordingCtx {
    fn size(&self) -> phi_ext::DrawSize {
        phi_ext::DrawSize { cols: 80, rows: 24 }
    }
    fn put_text(
        &mut self,
        col: i32,
        row: i32,
        _max_cols: i32,
        text: &str,
        _fg: phi_ext::Rgb,
        _bg: phi_ext::Rgb,
    ) {
        self.texts.push((col, row, text.to_owned(), false));
    }
    fn put_text_bold(
        &mut self,
        col: i32,
        row: i32,
        _max_cols: i32,
        text: &str,
        _fg: phi_ext::Rgb,
        _bg: phi_ext::Rgb,
    ) {
        self.texts.push((col, row, text.to_owned(), true));
    }
    fn fill_rect(&mut self, _rect: phi_ext::DrawRect, _fg: phi_ext::Rgb, _bg: phi_ext::Rgb) {}
    fn draw_box(
        &mut self,
        rect: phi_ext::DrawRect,
        _fg: phi_ext::Rgb,
        _bg: phi_ext::Rgb,
        _border: phi_ext::Rgb,
    ) {
        self.boxes.push(rect);
    }
    fn set_cell(
        &mut self,
        _col: i32,
        _row: i32,
        _text: &str,
        _fg: phi_ext::Rgb,
        _bg: phi_ext::Rgb,
        _underline: bool,
        _reverse: bool,
    ) {
    }
    #[allow(clippy::too_many_arguments)]
    fn put_cell_span(
        &mut self,
        _col: i32,
        _row: i32,
        _span: i32,
        _text: &str,
        _fg: phi_ext::Rgb,
        _bg: phi_ext::Rgb,
        _underline: bool,
        _reverse: bool,
        _bold: bool,
    ) {
    }
    #[allow(clippy::too_many_arguments)]
    fn render_scrollbar(
        &mut self,
        _col: i32,
        _row: i32,
        _rows: i32,
        _visible_items: usize,
        _total_items: usize,
        _scroll_from_top: usize,
        _fg: phi_ext::Rgb,
        _bg: phi_ext::Rgb,
        _track_glyph: &str,
        _thumb_fg: phi_ext::Rgb,
        _thumb_glyph: &str,
    ) {
        self.scrollbars += 1;
    }
    fn clear(&mut self, _bg: phi_ext::Rgb) {}
    fn invalidate(&mut self) {}
}

fn key(code: phi_ext::OverlayKeyCode) -> phi_ext::OverlayKey {
    phi_ext::OverlayKey {
        code,
        modifiers: phi_ext::OverlayKeyModifiers::default(),
    }
}

/// The WS1.6 dogfood: a Lua port of the builtin help overlay — payload into
/// `init`, palette-driven draw ops, Lua-owned key handling with scroll state,
/// wheel scrolling. Every capability here was unreachable from Lua before
/// (fixed text-box renderer, fixed Esc/Enter/q keys, no payload, no state).
#[tokio::test]
async fn lua_overlay_ports_builtin_help() {
    let source = r#"
phi.register_overlay("lua-help", {
    description = "Keybinding reference (Lua port of phi:help)",
    init = function(payload)
        local entries = (payload and payload.entries) or {}
        return { entries = entries, scroll = 0 }
    end,
    render = function(state, ctx)
        local ops = {
            { op = "box", col = 0, row = 0, cols = ctx.cols, rows = ctx.rows,
              bg = "surface_raised", border = "statusbar_bg" },
            { op = "text", col = 2, row = 1, text = " HELP ", bold = true, fg = "accent" },
        }
        local row = 3
        for i = state.scroll + 1, #state.entries do
            local e = state.entries[i]
            if e.heading then
                ops[#ops + 1] = { op = "text", col = 2, row = row, text = e.heading, bold = true }
            elseif e.keys then
                ops[#ops + 1] = { op = "text", col = 2, row = row, text = e.keys, fg = "accent_2" }
                ops[#ops + 1] = { op = "text", col = 27, row = row, text = e.description, fg = "muted" }
            end
            row = row + 1
        end
        ops[#ops + 1] = { op = "scrollbar", col = ctx.cols - 1, row = 3, rows = ctx.rows - 4,
                          visible = ctx.rows - 4, total = #state.entries, scroll = state.scroll }
        return ops
    end,
    handle_key = function(state, key, cols, rows)
        if key.code == "esc" or (key.code == "char" and key.char == "q") then
            return "close"
        end
        if key.code == "down" or (key.code == "char" and key.char == "j") then
            state.scroll = math.min(state.scroll + 1, #state.entries - 1)
        elseif key.code == "up" or (key.code == "char" and key.char == "k") then
            state.scroll = math.max(state.scroll - 1, 0)
        end
        return nil
    end,
    handle_scroll = function(state, delta, cols, rows)
        state.scroll = math.max(0, math.min(state.scroll + delta, #state.entries - 1))
    end,
})
"#;
    let rt = load_lua(source);
    let overlay = rt.get_overlay("lua-help").unwrap();

    // Open with the same typed payload the host hands the builtin overlay.
    let payload: phi_ext::OverlayPayload = Box::new(phi_ext::HelpPayload {
        entries: vec![
            phi_ext::HelpEntry::Heading("Global".to_owned()),
            phi_ext::HelpEntry::Item {
                keys: "ctrl+g".to_owned(),
                description: "toggle help".to_owned(),
            },
        ],
    });
    let mut state = (overlay.init_state)(Some(payload));

    let mut ctx = RecordingCtx::default();
    (overlay.render)(&mut ctx, &mut state);
    let text = ctx.all_text();
    assert!(text.contains("HELP"), "{text}");
    assert!(text.contains("Global"), "{text}");
    assert!(text.contains("ctrl+g"), "{text}");
    assert!(text.contains("toggle help"), "{text}");
    assert_eq!(ctx.boxes.len(), 1);
    assert_eq!(ctx.boxes[0].cols, 80);
    assert_eq!(ctx.scrollbars, 1);
    // The op-based path replaces the legacy fixed box (no footer hint).
    assert!(!text.contains("Esc/Enter/q to close"), "{text}");

    // Lua-owned key handling mutates Lua-owned state: `j` scrolls the
    // heading off.
    let outcome =
        (overlay.handle_key)(&mut state, &key(phi_ext::OverlayKeyCode::Char('j')), 80, 24);
    assert_eq!(outcome, phi_ext::OverlayOutcome::None);
    let mut ctx = RecordingCtx::default();
    (overlay.render)(&mut ctx, &mut state);
    let text = ctx.all_text();
    assert!(!text.contains("Global"), "{text}");
    assert!(text.contains("ctrl+g"), "{text}");

    // Wheel scrolling reaches Lua too.
    (overlay.handle_scroll.as_ref().unwrap())(&mut state, -1, 80, 24);
    let mut ctx = RecordingCtx::default();
    (overlay.render)(&mut ctx, &mut state);
    assert!(ctx.all_text().contains("Global"));

    // Lua decides the close keys.
    assert_eq!(
        (overlay.handle_key)(&mut state, &key(phi_ext::OverlayKeyCode::Esc), 80, 24),
        phi_ext::OverlayOutcome::Close
    );
}

/// A Lua model picker: the typed `ModelPickerPayload` crosses as a plain
/// table, the response channel stays host-side, `{ respond = ... }` fires it,
/// and a dismissed picker still answers `Ok(None)` via reply-on-drop.
#[tokio::test]
async fn lua_overlay_model_picker_responder() {
    use std::sync::{Arc as StdArc, Mutex};

    let source = r#"
phi.register_overlay("lua-model", {
    description = "Model picker (Lua port of phi:model)",
    init = function(payload)
        return { models = payload.models, sel = 1 }
    end,
    render = function(state, ctx)
        local ops = {}
        for i, m in ipairs(state.models) do
            ops[#ops + 1] = { op = "text", col = 2, row = i, text = m,
                              fg = (i == state.sel) and "accent" or "fg" }
        end
        return ops
    end,
    handle_key = function(state, key, cols, rows)
        if key.code == "down" then
            state.sel = math.min(state.sel + 1, #state.models)
        elseif key.code == "enter" then
            return { respond = state.models[state.sel] }
        elseif key.code == "esc" then
            return "close"
        end
        return nil
    end,
})
"#;
    let rt = load_lua(source);
    let overlay = rt.get_overlay("lua-model").unwrap();

    let picker_payload = |result: &StdArc<Mutex<Option<phi_ext::OverlayResponse>>>| {
        let result = StdArc::clone(result);
        Box::new(phi_ext::ModelPickerPayload {
            root: true,
            current: "prov/a".to_owned(),
            models: vec!["prov/a".to_owned(), "prov/b".to_owned()],
            responder: phi_ext::OverlayResponder::new(move |r| {
                *result.lock().unwrap() = Some(r);
            }),
        }) as phi_ext::OverlayPayload
    };

    // Selection round-trip: Down then Enter responds with the second model.
    let result = StdArc::new(Mutex::new(None));
    let mut state = (overlay.init_state)(Some(picker_payload(&result)));
    (overlay.handle_key)(&mut state, &key(phi_ext::OverlayKeyCode::Down), 80, 24);
    let outcome = (overlay.handle_key)(&mut state, &key(phi_ext::OverlayKeyCode::Enter), 80, 24);
    assert_eq!(outcome, phi_ext::OverlayOutcome::Close);
    assert_eq!(
        result.lock().unwrap().clone(),
        Some(Ok(Some("prov/b".to_owned())))
    );

    // Dismissal: dropping the state (host closes the overlay) fires Ok(None)
    // — the reply-on-drop contract survives the Lua bridge.
    let result = StdArc::new(Mutex::new(None));
    let state = (overlay.init_state)(Some(picker_payload(&result)));
    drop(state);
    assert_eq!(result.lock().unwrap().clone(), Some(Ok(None)));
}

/// A Lua overlay can hand the host a `Complete(CommandOutput)` — the same
/// message + `UiAction` vocabulary command handlers return (the archive
/// overlay's contract, WS1.7 part B).
#[tokio::test]
async fn lua_overlay_complete_outcome() {
    let source = r#"
phi.register_overlay("lua-actions", {
    description = "Completes with host actions",
    render = function(state, ctx) return {} end,
    handle_key = function(state, key, cols, rows)
        if key.code == "enter" then
            return { message = "restored", actions = { { type = "RunHostAction", data = { name = "reload" } } } }
        end
        return nil
    end,
})
"#;
    let rt = load_lua(source);
    let overlay = rt.get_overlay("lua-actions").unwrap();
    let mut state = (overlay.init_state)(None);
    let outcome = (overlay.handle_key)(&mut state, &key(phi_ext::OverlayKeyCode::Enter), 80, 24);
    assert_eq!(
        outcome,
        phi_ext::OverlayOutcome::Complete(phi_ext::CommandOutput {
            message: Some("restored".to_owned()),
            actions: vec![phi_ext::UiAction::RunHostAction {
                name: "reload".to_owned(),
            }],
        })
    );
}

/// Pre-WS1.6 overlays keep working: line-array render draws the legacy
/// bordered text box, and without `handle_key` the host-side Esc/Enter/q
/// close handler applies.
#[tokio::test]
async fn lua_overlay_legacy_lines_still_work() {
    let source = r#"
phi.register_overlay("legacy", {
    description = "Old-style overlay",
    render = function()
        return { "line one", "line two" }
    end
})
"#;
    let rt = load_lua(source);
    let overlay = rt.get_overlay("legacy").unwrap();
    let mut state = (overlay.init_state)(None);

    let mut ctx = RecordingCtx::default();
    (overlay.render)(&mut ctx, &mut state);
    let text = ctx.all_text();
    assert!(text.contains("line one"), "{text}");
    assert!(text.contains("line two"), "{text}");
    assert!(text.contains("Esc/Enter/q to close"), "{text}");
    assert_eq!(ctx.boxes.len(), 1);

    for code in [
        phi_ext::OverlayKeyCode::Esc,
        phi_ext::OverlayKeyCode::Enter,
        phi_ext::OverlayKeyCode::Char('q'),
    ] {
        assert_eq!(
            (overlay.handle_key)(&mut state, &key(code), 80, 24),
            phi_ext::OverlayOutcome::Close
        );
    }
    assert!(overlay.handle_scroll.is_none());
}

/// A generic `UiAction::OpenOverlay` JSON payload reaches Lua `init` as a
/// plain table.
#[tokio::test]
async fn lua_overlay_receives_json_payload() {
    let source = r#"
phi.register_overlay("json-payload", {
    description = "Echoes its payload",
    init = function(payload)
        return { greeting = payload and payload.greeting or "none" }
    end,
    render = function(state, ctx)
        return { { op = "text", col = 0, row = 0, text = state.greeting } }
    end,
})
"#;
    let rt = load_lua(source);
    let overlay = rt.get_overlay("json-payload").unwrap();

    let payload: phi_ext::OverlayPayload = Box::new(serde_json::json!({ "greeting": "hello" }));
    let mut state = (overlay.init_state)(Some(payload));
    let mut ctx = RecordingCtx::default();
    (overlay.render)(&mut ctx, &mut state);
    assert!(ctx.all_text().contains("hello"));

    // Toggle-key opens carry no payload.
    let mut state = (overlay.init_state)(None);
    let mut ctx = RecordingCtx::default();
    (overlay.render)(&mut ctx, &mut state);
    assert!(ctx.all_text().contains("none"));
}

// ── Tool renderers (WS1.3a) ─────────────────────────────────────────────

#[tokio::test]
async fn lua_register_tool_renderer_works() {
    // Dogfood test: the full declarative vocabulary a builtin spec uses
    // (verb, display name, category, styled anchor args, code body, result
    // lang, exit trailer) must round-trip from a Lua table.
    let source = r#"
phi.register_tool_renderer("deploy", {
    verb = "deploy",
    display_name = "Deploy",
    category = "commands",
    anchor = {
        { key = "target", style = "pattern" },
        { key = "env", style = "path", prefix = "to", skip_values = { "prod" } },
    },
    body = { kind = "code", key = "script", lang = "bash", prompt = "$" },
    result_lang = { kind = "fixed", lang = "json" },
    exit_code_trailer = true,
})
"#;
    let rt = load_lua(source);
    let idx = rt.tool_render_index();
    let spec = idx.get("deploy").expect("lua-registered spec present");
    assert_eq!(spec.verb.as_deref(), Some("deploy"));
    assert_eq!(spec.resolved_display_name(), "Deploy");
    assert_eq!(
        idx.category("deploy"),
        phi_ext::ToolRenderCategory::Commands
    );
    assert_eq!(spec.anchor.len(), 2);
    assert_eq!(spec.anchor[0].key, "target");
    assert_eq!(spec.anchor[0].style, phi_ext::AnchorStyle::Pattern);
    assert_eq!(spec.anchor[1].prefix.as_deref(), Some("to"));
    assert!(spec.anchor[1].skips("prod"));
    assert!(matches!(
        &spec.body,
        phi_ext::ToolBody::Code { key, lang, prompt }
            if key == "script" && lang.as_deref() == Some("bash") && prompt.as_deref() == Some("$")
    ));
    assert!(matches!(
        &spec.result_lang,
        phi_ext::ResultLang::Fixed { lang } if lang == "json"
    ));
    assert!(spec.exit_code_trailer);
}

#[tokio::test]
async fn lua_tool_renderer_overrides_core_repl_seed() {
    // Last-write-wins: a user extension can restyle even the core repl spec.
    let source = r#"
phi.register_tool_renderer("repl", { display_name = "Sandbox" })
"#;
    let rt = load_lua(source);
    let idx = rt.tool_render_index();
    assert_eq!(idx.get("repl").unwrap().resolved_display_name(), "Sandbox");
}

#[tokio::test]
async fn lua_tool_renderer_defaults_fill_in() {
    // A minimal spec: everything optional defaults (generic anchor form,
    // args body, "other" category).
    let source = r#"
phi.register_tool_renderer("mystery", {})
"#;
    let rt = load_lua(source);
    let idx = rt.tool_render_index();
    let spec = idx.get("mystery").unwrap();
    assert!(spec.verb.is_none());
    assert_eq!(spec.resolved_display_name(), "Mystery");
    assert_eq!(idx.category("mystery"), phi_ext::ToolRenderCategory::Other);
    assert!(matches!(spec.body, phi_ext::ToolBody::Args));
    assert!(!spec.exit_code_trailer);
}

// ── phi.ui — ask-the-user parity (WS7.2) ────────────────────────────────────

/// Dogfood: the approvals-extension shape, ported to Lua. A `tool_call`
/// handler with a raised `timeout` blocks on `phi.ui.ask` and gates the
/// call; without a connected host it fails closed via `phi.ui.has_ui()`.
#[tokio::test]
async fn lua_ui_ask_gates_tool_call() {
    let source = r#"
phi.on("tool_call", function(payload)
    if payload.data.name ~= "bash" then return nil end
    if not phi.ui.has_ui() then
        return { block = true, reason = "no ui - denied" }
    end
    local choice, outcome = phi.ui.ask({
        title = "Allow bash?",
        body = payload.data.args.cmd,
        options = { "Allow", { label = "Deny", detail = "block it" } },
        wait = 10,
    })
    if choice == 1 then return nil end
    return { block = true, reason = "denied via lua: " .. outcome }
end, { timeout = 15 })
"#;
    let rt = load_lua(source);
    let bash_call = || LifecycleEvent {
        kind: EventKind::ToolCall,
        payload: phi_ext::EventPayload::ToolCall {
            name: "bash".to_owned(),
            args: serde_json::json!({"cmd": "rm -rf /tmp/x"}),
        },
    };

    // No host connected: the Lua gate fails closed.
    let returns = rt.dispatch(bash_call()).await.unwrap();
    assert!(
        matches!(&returns[..], [EventReturn::ToolBlock { reason }]
            if reason.contains("no ui")),
        "expected fail-closed block, got {returns:?}"
    );

    // Connect a host; it answers Deny (host index 1 → Lua choice 2), then
    // Allow (host index 0 → Lua choice 1).
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    rt.connect_ui(tx);
    let host = tokio::spawn(async move {
        let req = rx.recv().await.expect("prompt 1");
        assert_eq!(req.prompt.title, "Allow bash?");
        assert_eq!(req.prompt.body, "rm -rf /tmp/x");
        assert_eq!(req.prompt.options.len(), 2);
        assert_eq!(req.prompt.options[0].label, "Allow");
        assert_eq!(req.prompt.options[1].label, "Deny");
        assert_eq!(req.prompt.options[1].detail.as_deref(), Some("block it"));
        req.reply.send(Some(1)).unwrap();

        let req = rx.recv().await.expect("prompt 2");
        req.reply.send(Some(0)).unwrap();
    });

    let returns = rt.dispatch(bash_call()).await.unwrap();
    assert!(
        matches!(&returns[..], [EventReturn::ToolBlock { reason }]
            if reason.contains("denied via lua: answer")),
        "expected user deny, got {returns:?}"
    );
    let returns = rt.dispatch(bash_call()).await.unwrap();
    assert!(
        returns.is_empty(),
        "allowed call must not block: {returns:?}"
    );
    host.await.unwrap();

    // Tools the Lua gate doesn't cover pass untouched.
    let returns = rt
        .dispatch(LifecycleEvent {
            kind: EventKind::ToolCall,
            payload: phi_ext::EventPayload::ToolCall {
                name: "read_file".to_owned(),
                args: serde_json::json!({"path": "x"}),
            },
        })
        .await
        .unwrap();
    assert!(returns.is_empty());
}

// ── phi.json + host-injected globals ────────────────────────────────────────

/// `phi.json` round-trips through serde_json — the same parser the host
/// uses — with `null` → `nil` decode semantics.
#[tokio::test]
async fn lua_phi_json_round_trips() {
    let source = r#"
phi.register_command("json-echo", {
    description = "decode then re-encode the argument",
    handler = function(args)
        local v, err = phi.json.decode(args)
        if err then error("decode failed: " .. err) end
        return phi.json.encode(v)
    end
})
phi.register_command("json-null-is-nil", {
    description = "JSON null must decode to Lua nil",
    handler = function()
        local v = phi.json.decode('{"a":null,"b":1}')
        if v.a == nil and v.b == 1 then return "ok" end
        return "bad: " .. tostring(v.a)
    end
})
phi.register_command("json-bad-input", {
    description = "malformed JSON returns nil + err, not a crash",
    handler = function()
        local v, err = phi.json.decode('{"a":')
        if v == nil and err ~= nil then return "ok" end
        return "bad"
    end
})
"#;
    let rt = load_lua(source);

    let out = rt
        .invoke_command(
            "json-echo",
            r#"{"tool":"bash","n":2,"ok":true,"list":["a","b"]}"#,
        )
        .await
        .unwrap()
        .unwrap();
    let echoed: serde_json::Value = serde_json::from_str(&out.message.unwrap()).unwrap();
    assert_eq!(
        echoed,
        serde_json::json!({"tool": "bash", "n": 2, "ok": true, "list": ["a", "b"]})
    );

    for cmd in ["json-null-is-nil", "json-bad-input"] {
        let out = rt.invoke_command(cmd, "").await.unwrap().unwrap();
        assert_eq!(out.message.as_deref(), Some("ok"), "{cmd}");
    }
}

/// `phi.json.encode` raises a hard, named error on values JSON can't carry.
#[tokio::test]
async fn lua_phi_json_encode_rejects_functions() {
    let source = r#"
phi.register_command("json-encode-fn", {
    description = "encoding a function must raise",
    handler = function()
        local ok, err = pcall(phi.json.encode, { f = function() end })
        if not ok then return "raised" end
        return "encoded?!"
    end
})
"#;
    let rt = load_lua(source);
    let out = rt
        .invoke_command("json-encode-fn", "")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(out.message.as_deref(), Some("raised"));
}

/// `with_global` injects typed values (arrays, tables, booleans) that the
/// source sees as real Lua values before it runs — the host→extension
/// config channel the session builder uses for `_phi_*` globals.
#[tokio::test]
async fn lua_with_global_injects_typed_values() {
    let source = r#"
phi.register_command("read-globals", {
    description = "echo the injected globals",
    handler = function()
        local parts = {
            _g_str,
            tostring(_g_flag),
            tostring(#_g_list),
            _g_list[1],
            _g_table.inner,
            tostring(_g_table.missing == nil),
        }
        return table.concat(parts, "|")
    end
})
"#;
    let ext = {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("globals_ext.lua");
        std::fs::write(&path, source).unwrap();
        std::mem::forget(dir);
        LuaExtension::from_file(&path).unwrap()
    }
    .with_global("_g_str", serde_json::json!("hello"))
    .with_global("_g_flag", serde_json::json!(true))
    .with_global("_g_list", serde_json::json!(["a", "b", "c"]))
    .with_global(
        "_g_table",
        serde_json::json!({"inner": "x", "missing": null}),
    );
    let rt = RuntimeBuilder::new()
        .with_cwd(std::path::Path::new("."))
        .register_extension(ext)
        .build()
        .unwrap();

    let out = rt
        .invoke_command("read-globals", "")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(out.message.as_deref(), Some("hello|true|3|a|x|true"));
}

/// `phi.fs.canonicalize` resolves symlinks and returns nil for paths that
/// don't resolve.
#[tokio::test]
async fn lua_phi_fs_canonicalize() {
    let source = r#"
phi.register_command("canon", {
    description = "canonicalize the argument path",
    handler = function(args)
        local resolved = phi.fs.canonicalize(args)
        if resolved then return resolved end
        return "<nil>"
    end
})
"#;
    let rt = load_lua(source);

    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let out = rt
        .invoke_command("canon", &link.display().to_string())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        out.message.unwrap(),
        real.canonicalize().unwrap().display().to_string()
    );

    let out = rt
        .invoke_command("canon", "/definitely/not/a/path")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(out.message.as_deref(), Some("<nil>"));
}
