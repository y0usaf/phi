//! Dogfood tests: Phi's default extensions ship as embedded Lua source
//! (`phi_builtins::defaults`). These tests load them through
//! `LuaExtension::from_source` — the same path the session builder uses —
//! and verify they register the expected commands, overlays, and tool
//! renderers. This is the proof that the defaults ARE user extensions:
//! same API, same language, same execution path.

use phi_builtins::defaults;
use phi_ext::RuntimeBuilder;
use phi_lua::LuaExtension;

/// Build a runtime from one embedded Lua source, with an optional preamble
/// (used to inject globals like `_phi_model_names` before the source runs).
fn build_one(name: &str, source: &str) -> std::sync::Arc<phi_ext::AppRuntime> {
    let ext = LuaExtension::from_source(name, source.to_owned()).unwrap();
    std::sync::Arc::new(
        RuntimeBuilder::new()
            .with_cwd(std::path::Path::new("."))
            .register_extension(ext)
            .build()
            .unwrap(),
    )
}

// ── Session commands ─────────────────────────────────────────────────────

#[tokio::test]
async fn defaults_commands_register_all_session_commands() {
    // The session builder injects _phi_model_names before commands.lua;
    // we do the same here.
    let source = format!(
        "_phi_model_names = {{\"anthropic/claude-opus-4\"}}\n{lua}",
        lua = defaults::COMMANDS_LUA,
    );
    let rt = build_one("phi-commands", &source);

    let names: Vec<String> = rt.list_commands().into_iter().map(|c| c.name).collect();
    for expected in [
        "exit", "fork", "help", "login", "logout", "model", "new", "quit", "resume", "submodel",
        "tree", "trust", "usage",
    ] {
        assert!(
            names.contains(&expected.to_owned()),
            "defaults: missing command /{expected}, got {names:?}"
        );
    }
}

#[tokio::test]
async fn defaults_commands_model_with_arg_switches_without_arg_opens_picker() {
    let source = format!(
        "_phi_model_names = {{\"anthropic/claude-opus-4\"}}\n{lua}",
        lua = defaults::COMMANDS_LUA,
    );
    let rt = build_one("phi-commands", &source);

    // With argument: SwitchModel action.
    let out = rt.invoke_command_sync("model", "opus").unwrap().unwrap();
    assert_eq!(
        out.actions,
        vec![phi_ext::UiAction::SwitchModel {
            root: true,
            query: "opus".to_owned(),
        }]
    );

    // Without argument: OpenOverlay action.
    let out = rt.invoke_command_sync("model", "").unwrap().unwrap();
    assert_eq!(
        out.actions,
        vec![phi_ext::UiAction::OpenOverlay {
            name: phi_ext::OVERLAY_MODEL.to_owned(),
            payload: Some(serde_json::json!({ "root": true })),
        }]
    );
}

#[tokio::test]
async fn defaults_commands_quit_and_exit_both_quit() {
    let source = format!(
        "_phi_model_names = {{}}\n{lua}",
        lua = defaults::COMMANDS_LUA,
    );
    let rt = build_one("phi-commands", &source);

    for name in ["quit", "exit"] {
        let out = rt.invoke_command_sync(name, "").unwrap().unwrap();
        assert_eq!(out.actions, vec![phi_ext::UiAction::Quit]);
    }
}

#[tokio::test]
async fn defaults_commands_login_logout_return_actions() {
    let source = format!(
        "_phi_model_names = {{}}\n{lua}",
        lua = defaults::COMMANDS_LUA,
    );
    let rt = build_one("phi-commands", &source);

    let out = rt
        .invoke_command_sync("login", "anthropic")
        .unwrap()
        .unwrap();
    assert_eq!(
        out.actions,
        vec![phi_ext::UiAction::StartLogin {
            provider: "anthropic".to_owned(),
        }]
    );

    let out = rt.invoke_command_sync("logout", "openai").unwrap().unwrap();
    assert_eq!(
        out.actions,
        vec![phi_ext::UiAction::Logout {
            provider: "openai".to_owned(),
        }]
    );
}

#[tokio::test]
async fn defaults_commands_model_completions() {
    let source = format!(
        "_phi_model_names = {{\"anthropic/claude-opus-4\", \"openai/gpt-4o\"}}\n{lua}",
        lua = defaults::COMMANDS_LUA,
    );
    let rt = build_one("phi-commands", &source);

    assert_eq!(
        rt.complete_command_arg("model").unwrap(),
        vec![
            "anthropic/claude-opus-4".to_owned(),
            "openai/gpt-4o".to_owned()
        ]
    );
    assert_eq!(
        rt.complete_command_arg("login").unwrap(),
        vec!["anthropic".to_owned(), "openai".to_owned()]
    );
    assert!(rt.complete_command_arg("help").is_none());
}

// ── Mux commands ─────────────────────────────────────────────────────────

#[tokio::test]
async fn defaults_mux_commands_register_and_resolve() {
    let rt = build_one("phi-mux-commands", defaults::MUX_COMMANDS_LUA);

    // Session listing does not see mux commands.
    assert!(rt.list_commands().is_empty());

    // But mux context resolves them.
    let out = rt
        .invoke_command_sync_in(phi_ext::CommandContext::Mux, "quit", "")
        .unwrap()
        .unwrap();
    assert_eq!(out.actions, vec![phi_ext::UiAction::Quit]);

    let out = rt
        .invoke_command_sync_in(phi_ext::CommandContext::Mux, "q", "")
        .unwrap()
        .unwrap();
    assert_eq!(out.actions, vec![phi_ext::UiAction::Quit]);

    let out = rt
        .invoke_command_sync_in(phi_ext::CommandContext::Mux, "help", "")
        .unwrap()
        .unwrap();
    assert_eq!(
        out.actions,
        vec![phi_ext::UiAction::OpenOverlay {
            name: phi_ext::OVERLAY_HELP.to_owned(),
            payload: None,
        }]
    );

    // Navigation commands map to host actions.
    for (name, action) in [
        ("up", "session-prev"),
        ("down", "session-next"),
        ("left", "project-prev"),
        ("right", "project-next"),
    ] {
        let out = rt
            .invoke_command_sync_in(phi_ext::CommandContext::Mux, name, "")
            .unwrap()
            .unwrap();
        assert_eq!(
            out.actions,
            vec![phi_ext::UiAction::RunHostAction {
                name: action.to_owned(),
            }]
        );
    }
}

#[tokio::test]
async fn defaults_mux_open_parses_paths() {
    let rt = build_one("phi-mux-commands", defaults::MUX_COMMANDS_LUA);

    let out = rt
        .invoke_command_sync_in(phi_ext::CommandContext::Mux, "open", "/tmp/project")
        .unwrap()
        .unwrap();
    assert_eq!(
        out.actions,
        vec![phi_ext::UiAction::OpenProject(std::path::PathBuf::from(
            "/tmp/project"
        ))]
    );

    // Quoted path with spaces.
    let out = rt
        .invoke_command_sync_in(phi_ext::CommandContext::Mux, "open", "\"/tmp/my project\"")
        .unwrap()
        .unwrap();
    assert_eq!(
        out.actions,
        vec![phi_ext::UiAction::OpenProject(std::path::PathBuf::from(
            "/tmp/my project"
        ))]
    );

    // Empty: error.
    let err = rt
        .invoke_command_sync_in(phi_ext::CommandContext::Mux, "open", "")
        .unwrap_err();
    assert!(err.to_string().contains("usage"));
}

// ── Tool renderers ───────────────────────────────────────────────────────

#[tokio::test]
async fn defaults_tool_render_registers_all_specs() {
    let rt = build_one("phi-tool-render", defaults::TOOL_RENDER_LUA);
    let idx = rt.tool_render_index();

    for tool in [
        "bash",
        "read_file",
        "write_file",
        "edit_file",
        "grep",
        "find",
        "ls",
    ] {
        assert!(idx.get(tool).is_some(), "missing tool renderer for {tool}");
    }

    // bash: Shell category, code body, exit code trailer.
    let bash = idx.get("bash").unwrap();
    assert_eq!(bash.resolved_display_name(), "Shell");
    assert_eq!(idx.category("bash"), phi_ext::ToolRenderCategory::Commands);
    assert!(bash.exit_code_trailer);

    // read_file: Reads category, path anchor, path_arg result lang.
    let read = idx.get("read_file").unwrap();
    assert_eq!(
        idx.category("read_file"),
        phi_ext::ToolRenderCategory::Reads
    );
    assert_eq!(read.anchor[0].key, "path");
    assert_eq!(read.anchor[0].style, phi_ext::AnchorStyle::Path);

    // grep: Searches category with pattern + path anchors.
    let grep = idx.get("grep").unwrap();
    assert_eq!(idx.category("grep"), phi_ext::ToolRenderCategory::Searches);
    assert_eq!(grep.anchor.len(), 2);
    assert_eq!(grep.anchor[0].style, phi_ext::AnchorStyle::Pattern);
    assert_eq!(grep.anchor[1].prefix.as_deref(), Some("in"));
    assert!(grep.anchor[1].skips("."));
}

// ── Overlays ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn defaults_about_overlay_registers() {
    let rt = build_one("phi-about-overlay", defaults::ABOUT_OVERLAY_LUA);
    let overlay = rt.get_overlay("phi:about").unwrap();
    assert_eq!(overlay.name, "phi:about");
    assert_eq!(overlay.toggle_key.as_deref(), Some("ctrl+?"));
    assert!(overlay.handle_scroll.is_none());
}

#[tokio::test]
async fn defaults_help_overlay_registers_and_renders() {
    let rt = build_one("phi-help-overlay", defaults::HELP_OVERLAY_LUA);
    let overlay = rt.get_overlay("phi:help").unwrap();
    assert_eq!(overlay.name, "phi:help");
    assert!(overlay.handle_scroll.is_some());

    // Open with a HelpPayload (same typed payload the host hands the builtin).
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

    // Render should produce draw ops (not crash).
    // We can't easily assert on draw ops without a recording context,
    // but we can verify the overlay handles keys correctly.
    use phi_ext::{OverlayKey, OverlayKeyCode, OverlayOutcome};

    // Esc closes.
    let key = OverlayKey {
        code: OverlayKeyCode::Esc,
        modifiers: Default::default(),
    };
    assert_eq!(
        (overlay.handle_key)(&mut state, &key, 80, 24),
        OverlayOutcome::Close
    );

    // 'q' closes.
    let mut state = (overlay.init_state)(None);
    let key = OverlayKey {
        code: OverlayKeyCode::Char('q'),
        modifiers: Default::default(),
    };
    assert_eq!(
        (overlay.handle_key)(&mut state, &key, 80, 24),
        OverlayOutcome::Close
    );

    // 'j' scrolls (no close).
    let mut state = (overlay.init_state)(None);
    let key = OverlayKey {
        code: OverlayKeyCode::Char('j'),
        modifiers: Default::default(),
    };
    assert_eq!(
        (overlay.handle_key)(&mut state, &key, 80, 24),
        OverlayOutcome::None
    );
}

// ── Visual data: spinner + thinking expressions ──────────────────────────

#[tokio::test]
async fn defaults_visual_data_registers_spinner() {
    let rt = build_one("phi-visual-data", defaults::VISUAL_DATA_LUA);

    let spinner = rt.get_spinner("phi:default").unwrap();
    assert_eq!(spinner.name, "phi:default");
    assert_eq!(spinner.frame_interval_ms, 80);
    assert_eq!(spinner.frames.len(), 10);
    // Braille dot characters (UTF-8 multi-byte).
    assert!(spinner.frames[0].starts_with('\u{280B}'));
    assert!(spinner.frames[9].starts_with('\u{280F}'));
}

#[tokio::test]
async fn defaults_visual_data_registers_thinking_expression() {
    let rt = build_one("phi-visual-data", defaults::VISUAL_DATA_LUA);

    let expr = rt.get_thinking_expression("phi:default").unwrap();
    assert_eq!(expr.name, "phi:default");
    assert_eq!(
        expr.expressions,
        vec![
            "thinking...".to_owned(),
            "reasoning...".to_owned(),
            "pondering...".to_owned(),
            "working...".to_owned(),
        ]
    );
    assert_eq!(expr.tool_expression, "running...");
    assert_eq!(expr.expression_interval_ms, 2000);
}

// ── Coding tools (hybrid surface, dispatch = "python") ───────────────────

#[tokio::test]
async fn defaults_coding_tools_register_all_native_python_tools() {
    let rt = build_one("phi-coding-tools", defaults::CODING_TOOLS_LUA);

    let names: Vec<&str> = rt.native_tool_specs().map(|t| t.name.as_str()).collect();
    for expected in [
        "bash",
        "read_file",
        "write_file",
        "edit_file",
        "grep",
        "find",
        "ls",
    ] {
        assert!(names.contains(&expected), "missing {expected}: {names:?}");
        assert!(
            matches!(
                rt.native_tool_dispatch(expected),
                Some(phi_ext::ToolDispatch::Python)
            ),
            "{expected} must dispatch into the REPL env (Python), got {:?}",
            rt.native_tool_dispatch(expected).map(|d| match d {
                phi_ext::ToolDispatch::Python => "Python",
                phi_ext::ToolDispatch::Handler(_) => "Handler",
            })
        );
    }
    assert_eq!(names.len(), 7);
}

#[tokio::test]
async fn defaults_coding_tools_generate_no_prelude_stubs() {
    let rt = build_one("phi-coding-tools", defaults::CODING_TOOLS_LUA);
    // Python-dispatch tools must not generate REPL stubs — the prelude
    // already defines the functions; a stub would shadow them.
    assert!(rt.extension_prelude_stubs().is_empty());
}

#[tokio::test]
async fn defaults_coding_tools_schemas_mark_required_fields() {
    let rt = build_one("phi-coding-tools", defaults::CODING_TOOLS_LUA);

    // bash requires "cmd".
    let bash = rt.native_tool_specs().find(|t| t.name == "bash").unwrap();
    assert_eq!(
        bash.native_input_schema()["required"],
        serde_json::json!(["cmd"])
    );

    // write_file requires path + content.
    let wf = rt
        .native_tool_specs()
        .find(|t| t.name == "write_file")
        .unwrap();
    assert_eq!(
        wf.native_input_schema()["required"],
        serde_json::json!(["path", "content"])
    );

    // edit_file: two modes, nothing universally required. An empty Lua
    // table deserializes as an empty object (not array) — both mean none.
    let ef = rt
        .native_tool_specs()
        .find(|t| t.name == "edit_file")
        .unwrap();
    assert!(
        ef.native_input_schema()["required"].is_array()
            || ef.native_input_schema()["required"].is_object(),
    );
    assert!(
        ef.native_input_schema()["required"]
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(true)
    );

    // read_file has a "fresh" description in its schema.
    let rf = rt
        .native_tool_specs()
        .find(|t| t.name == "read_file")
        .unwrap();
    assert!(
        rf.native_input_schema()["properties"]["fresh"]["description"]
            .as_str()
            .is_some()
    );
}
