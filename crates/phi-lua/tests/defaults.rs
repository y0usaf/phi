//! Dogfood tests: Phi's default extensions ship as embedded Lua source
//! (`phi_builtins::defaults`). These tests load them through
//! `LuaExtension::from_source` — the same path the session builder uses —
//! and verify they register the expected commands, overlays, and tool
//! renderers. This is the proof that the defaults ARE user extensions:
//! same API, same language, same execution path.

use phi_builtins::defaults;
use phi_ext::RuntimeBuilder;
use phi_lua::LuaExtension;

/// Build a runtime from one embedded Lua source.
fn build_one(name: &str, source: &str) -> std::sync::Arc<phi_ext::AppRuntime> {
    build_one_with(name, source, &[])
}

/// Build a runtime from one embedded Lua source with host-injected globals —
/// the same `with_global` channel the session builder uses.
fn build_one_with(
    name: &str,
    source: &str,
    globals: &[(&str, serde_json::Value)],
) -> std::sync::Arc<phi_ext::AppRuntime> {
    let mut ext = LuaExtension::from_source(name, source.to_owned()).unwrap();
    for (key, value) in globals {
        ext = ext.with_global(key, value.clone());
    }
    std::sync::Arc::new(
        RuntimeBuilder::new()
            .with_cwd(std::path::Path::new("."))
            .register_extension(ext)
            .build()
            .unwrap(),
    )
}

// ── Session commands ─────────────────────────────────────────────────────

/// Commands runtime with `_phi_model_names` injected the same way the
/// session builder does (typed `with_global`, not source concatenation).
fn commands_runtime(models: &[&str]) -> std::sync::Arc<phi_ext::AppRuntime> {
    build_one_with(
        "phi-commands",
        defaults::COMMANDS_LUA,
        &[("_phi_model_names", serde_json::json!(models))],
    )
}

#[tokio::test]
async fn defaults_commands_register_all_session_commands() {
    let rt = commands_runtime(&["anthropic/claude-opus-4"]);

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
    let rt = commands_runtime(&["anthropic/claude-opus-4"]);

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
    let rt = commands_runtime(&[]);

    for name in ["quit", "exit"] {
        let out = rt.invoke_command_sync(name, "").unwrap().unwrap();
        assert_eq!(out.actions, vec![phi_ext::UiAction::Quit]);
    }
}

#[tokio::test]
async fn defaults_commands_login_logout_return_actions() {
    let rt = commands_runtime(&[]);

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
    let rt = commands_runtime(&["anthropic/claude-opus-4", "openai/gpt-4o"]);

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

#[tokio::test]
async fn defaults_visual_data_registers_phi_style() {
    let rt = build_one("phi-visual-data", defaults::VISUAL_DATA_LUA);

    // The `phi` default style is an empty bundle: activating it falls back
    // to the base registrations, so "switch back to phi" restores the
    // defaults without any special-casing.
    assert_eq!(rt.list_styles(), vec!["phi"]);
    rt.set_active_style(Some("phi")).unwrap();
    assert_eq!(rt.active_spinner().unwrap().name, "phi:default");
    assert_eq!(rt.active_thinking_expression().unwrap().name, "phi:default");
}

#[tokio::test]
async fn defaults_style_commands_emit_switch_style() {
    // /style (session) and :style (mux) both return the declarative
    // SwitchStyle action; the hosts interpret and persist it.
    let rt = commands_runtime(&[]);
    let output = rt
        .invoke_command("style", "crush")
        .await
        .unwrap()
        .expect("style command registered");
    assert_eq!(
        output.actions,
        vec![phi_ext::UiAction::SwitchStyle {
            name: "crush".to_owned()
        }]
    );

    let rt = build_one("phi-mux-commands", defaults::MUX_COMMANDS_LUA);
    let output = rt
        .invoke_command_sync_in(phi_ext::CommandContext::Mux, "style", "")
        .unwrap()
        .expect(":style command registered");
    assert_eq!(
        output.actions,
        vec![phi_ext::UiAction::SwitchStyle {
            name: String::new()
        }]
    );
}

// ── Style pack: claude-code (WS10.2) ─────────────────────────────────────

#[tokio::test]
async fn defaults_claude_code_style_registers_and_activates() {
    let rt = build_one("phi-style-claude-code", defaults::STYLE_CLAUDE_CODE_LUA);

    assert_eq!(rt.list_styles(), vec!["claude-code"]);
    rt.set_active_style(Some("claude-code")).unwrap();

    // Theme: claude orange accent, from ref/claude-code's darkTheme.
    let theme = rt.active_theme().unwrap();
    assert_eq!(
        theme.palette.get("accent"),
        Some(&phi_ext::Rgb(215, 119, 87))
    );

    // Spinner: the asterisk morph, ping-ponged at 120ms.
    let spinner = rt.active_spinner().unwrap();
    assert_eq!(spinner.frames.len(), 12);
    assert_eq!(spinner.frame_interval_ms, 120);
    assert_eq!(spinner.frames.first().map(String::as_str), Some("·"));
    assert_eq!(spinner.frames[5], spinner.frames[6]); // ping-pong midpoint

    // Thinking verbs: gerunds ending in the unicode ellipsis.
    let think = rt.active_thinking_expression().unwrap();
    assert!(think.expressions.iter().any(|e| e == "Clauding…"));
    assert!(think.expressions.iter().all(|e| e.ends_with('…')));

    // Tool renderers overlay the base index while the pack is active.
    let idx = rt.tool_render_index();
    assert_eq!(idx.get("bash").unwrap().verb.as_deref(), Some("Bash"));
    assert_eq!(
        idx.get("edit_file").unwrap().verb.as_deref(),
        Some("Update")
    );
    assert_eq!(idx.get("grep").unwrap().verb.as_deref(), Some("Search"));

    // Deactivating falls back to the base registrations.
    rt.set_active_style(None).unwrap();
    assert!(rt.tool_render_index().get("bash").is_none());
}

#[tokio::test]
async fn defaults_claude_code_style_restyles_every_base_tool() {
    // Every tool base tool_render.lua styles must be restyled by the pack,
    // or that tool renders phi-style while the pack is active.
    let rt = build_one("phi-style-claude-code", defaults::STYLE_CLAUDE_CODE_LUA);
    let pack = rt.get_style("claude-code").unwrap();
    for tool in [
        "bash",
        "read_file",
        "write_file",
        "edit_file",
        "grep",
        "find",
        "ls",
    ] {
        assert!(
            pack.tool_renderers.iter().any(|s| s.tool == tool),
            "claude-code pack misses a renderer for base-styled tool `{tool}`"
        );
    }
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

// ── Drift guards: defaults must describe documented prelude helpers ─────

/// Every tool renderer spec must describe a documented prelude helper —
/// the roster the model reads must match what the UI can render.
/// (`tool_render.lua` registers exactly these; the registration test above
/// fails if one goes missing, this fails if one leaves the prelude.)
#[tokio::test]
async fn defaults_tool_render_specs_match_prelude_roster() {
    let roster = phi_builtins::prelude::coding_tools_section();
    for tool in [
        "bash",
        "read_file",
        "write_file",
        "edit_file",
        "grep",
        "find",
        "ls",
    ] {
        assert!(
            roster.contains(&format!("- {tool}(")),
            "tool render spec `{tool}` is not a documented prelude helper — prelude drift"
        );
    }
}

#[tokio::test]
async fn defaults_tool_render_categories_cover_the_summary_vocabulary() {
    let rt = build_one("phi-tool-render", defaults::TOOL_RENDER_LUA);
    let idx = rt.tool_render_index();
    use phi_ext::ToolRenderCategory as C;
    assert_eq!(idx.category("read_file"), C::Reads);
    assert_eq!(idx.category("edit_file"), C::Edits);
    assert_eq!(idx.category("write_file"), C::Edits);
    assert_eq!(idx.category("bash"), C::Commands);
    assert_eq!(idx.category("grep"), C::Searches);
    assert_eq!(idx.category("find"), C::Searches);
    assert_eq!(idx.category("ls"), C::Other);
}

/// The native tool names must stay tied to real, documented prelude
/// functions — this one iterates what the Lua default actually registered.
#[tokio::test]
async fn defaults_coding_tools_are_documented_in_the_prelude_roster() {
    let rt = build_one("phi-coding-tools", defaults::CODING_TOOLS_LUA);
    let roster = phi_builtins::prelude::coding_tools_section();
    for spec in rt.native_tool_specs() {
        assert!(
            roster.contains(&format!("- {}(", spec.name)),
            "native tool `{}` is not a documented prelude helper — prompt/prelude drift",
            spec.name
        );
    }
}

// ── Mux :open path handling ──────────────────────────────────────────────

#[tokio::test]
async fn defaults_mux_open_expands_home_prefix() {
    let rt = build_one("phi-mux-commands", defaults::MUX_COMMANDS_LUA);
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("~"));
    let out = rt
        .invoke_command_sync_in(phi_ext::CommandContext::Mux, "open", "~/project")
        .unwrap()
        .unwrap();
    assert_eq!(
        out.actions,
        vec![phi_ext::UiAction::OpenProject(home.join("project"))]
    );
}

#[tokio::test]
async fn defaults_mux_open_unterminated_quote_errors() {
    let rt = build_one("phi-mux-commands", defaults::MUX_COMMANDS_LUA);
    let err = rt
        .invoke_command_sync_in(phi_ext::CommandContext::Mux, "open", "\"/tmp/half")
        .unwrap_err();
    assert!(err.to_string().contains("unterminated"), "{err}");
}

// ── Skills: discovery + system-prompt injection ──────────────────────────

fn skills_runtime(dir: &std::path::Path, include_project: bool) -> phi_ext::AppRuntime {
    let ext = LuaExtension::from_source("phi-skills", defaults::SKILLS_LUA.to_owned())
        .unwrap()
        .with_global(
            "_phi_skills_cwd",
            serde_json::json!(dir.display().to_string()),
        )
        .with_global("_phi_skills_extra_paths", serde_json::json!([]))
        .with_global(
            "_phi_skills_include_project",
            serde_json::json!(include_project),
        );
    RuntimeBuilder::new()
        .with_cwd(dir)
        .register_extension(ext)
        .build()
        .unwrap()
}

/// End-to-end through the extension API: the block lands in the system
/// prompt via `before_agent_start` → `system_prompt_append`, and repeated
/// filtering never accumulates (each turn starts from the original).
#[tokio::test]
async fn defaults_skills_append_block_via_before_agent_start() {
    let dir = tempfile::tempdir().unwrap();
    let my_skill = dir.path().join(".phi").join("skills").join("my-skill");
    std::fs::create_dir_all(&my_skill).unwrap();
    std::fs::write(
        my_skill.join("SKILL.md"),
        "---\nname: my-skill\ndescription: Does cool things.\n---\n",
    )
    .unwrap();

    let rt = skills_runtime(dir.path(), true);
    let base = "BASE PROMPT".to_owned();
    let filtered = rt
        .filter_system_prompt("user prompt", base.clone())
        .await
        .unwrap();
    assert!(filtered.starts_with("BASE PROMPT"));
    assert!(filtered.contains("<available_skills>"));
    assert!(filtered.contains("<name>my-skill</name>"));
    assert!(filtered.contains("<description>Does cool things.</description>"));

    // Second turn: same size, no accumulation.
    let again = rt
        .filter_system_prompt("user prompt", base.clone())
        .await
        .unwrap();
    assert_eq!(filtered, again);
}

/// An untrusted project's `.phi/skills` must not be scanned.
#[tokio::test]
async fn defaults_skills_untrusted_project_dir_is_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let my_skill = dir.path().join(".phi").join("skills").join("sneaky");
    std::fs::create_dir_all(&my_skill).unwrap();
    std::fs::write(
        my_skill.join("SKILL.md"),
        "---\nname: sneaky\ndescription: Should not load.\n---\n",
    )
    .unwrap();

    let rt = skills_runtime(dir.path(), false);
    let filtered = rt
        .filter_system_prompt("user prompt", "BASE".to_owned())
        .await
        .unwrap();
    assert!(!filtered.contains("sneaky"), "{filtered}");
}

// ── Approvals: the tool gate ─────────────────────────────────────────────

fn approvals_ext(dir: &std::path::Path, rules: serde_json::Value) -> LuaExtension {
    LuaExtension::from_source("phi-approvals", defaults::APPROVALS_LUA.to_owned())
        .unwrap()
        .with_global(
            "_phi_approvals_cwd",
            serde_json::json!(dir.display().to_string()),
        )
        .with_global("_phi_approvals_rules", rules)
        .with_global(
            "_phi_approvals_store_path",
            serde_json::json!(dir.join("approvals.json").display().to_string()),
        )
}

fn approvals_runtime(dir: &std::path::Path, rules: serde_json::Value) -> phi_ext::AppRuntime {
    RuntimeBuilder::new()
        .with_cwd(dir)
        .register_extension(approvals_ext(dir, rules))
        .build()
        .unwrap()
}

fn tool_call(name: &str, args: serde_json::Value) -> phi_ext::LifecycleEvent {
    phi_ext::LifecycleEvent {
        kind: phi_ext::EventKind::ToolCall,
        payload: phi_ext::EventPayload::ToolCall {
            name: name.to_owned(),
            args,
        },
    }
}

/// The store keys rules by canonical project path.
fn store_rules(dir: &std::path::Path) -> Vec<serde_json::Value> {
    let raw = std::fs::read_to_string(dir.join("approvals.json")).unwrap_or_default();
    if raw.is_empty() {
        return Vec::new();
    }
    let store: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let key = dir.canonicalize().unwrap().display().to_string();
    store
        .get(&key)
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
}

#[tokio::test]
async fn defaults_approvals_ask_without_ui_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let rt = approvals_runtime(dir.path(), serde_json::json!([]));
    let returns = rt
        .dispatch(tool_call("bash", serde_json::json!({"cmd": "rm -rf /"})))
        .await
        .unwrap();
    assert!(
        matches!(&returns[..], [phi_ext::EventReturn::ToolBlock { reason }]
            if reason.contains("no interactive UI")),
        "expected fail-closed block, got {returns:?}"
    );
    // Reads are not gated by default.
    let returns = rt
        .dispatch(tool_call("read_file", serde_json::json!({"path": "x"})))
        .await
        .unwrap();
    assert!(returns.is_empty());
}

#[tokio::test]
async fn defaults_approvals_config_deny_blocks_without_prompting() {
    let dir = tempfile::tempdir().unwrap();
    let rt = approvals_runtime(
        dir.path(),
        serde_json::json!([{"tool": "bash", "pattern": "git push*", "action": "deny"}]),
    );
    let returns = rt
        .dispatch(tool_call("bash", serde_json::json!({"cmd": "git push"})))
        .await
        .unwrap();
    assert!(
        matches!(&returns[..], [phi_ext::EventReturn::ToolBlock { reason }]
            if reason.contains("denied by an approvals rule"))
    );
}

#[tokio::test]
async fn defaults_approvals_invalid_config_action_fails_registration() {
    let dir = tempfile::tempdir().unwrap();
    let result = RuntimeBuilder::new()
        .with_cwd(dir.path())
        .register_extension(approvals_ext(
            dir.path(),
            serde_json::json!([{"tool": "bash", "action": "alow"}]),
        ))
        .build();
    assert!(result.is_err(), "typo'd action must not silently gate");
}

#[tokio::test]
async fn defaults_approvals_prompt_flow_allow_once_and_always() {
    let dir = tempfile::tempdir().unwrap();
    let rt = approvals_runtime(dir.path(), serde_json::json!([]));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    rt.connect_ui(tx);

    let host = tokio::spawn(async move {
        // Round 1: user picks "Allow once" (index 0).
        let req = rx.recv().await.expect("prompt 1");
        assert_eq!(req.prompt.title, "Allow bash?");
        assert_eq!(req.prompt.body, "cargo test");
        assert_eq!(req.prompt.options.len(), 3);
        req.reply.send(Some(0)).unwrap();

        // Round 2: user picks "Always allow bash" (index 1).
        let req = rx.recv().await.expect("prompt 2");
        req.reply.send(Some(1)).unwrap();

        // Round 4 below dismisses (reply None).
        let req = rx.recv().await.expect("prompt 4");
        req.reply.send(None).unwrap();
    });

    let call = || tool_call("bash", serde_json::json!({"cmd": "cargo test"}));
    // 1: allowed once — prompts.
    assert!(rt.dispatch(call()).await.unwrap().is_empty());
    // 2: "always allow" — prompts, persists.
    assert!(rt.dispatch(call()).await.unwrap().is_empty());
    // 3: covered by the persisted rule — no prompt consumed.
    assert!(rt.dispatch(call()).await.unwrap().is_empty());
    // The rule survives a store reload under the (canonical) project key.
    assert_eq!(store_rules(dir.path()).len(), 1);

    // 4: a differently-gated tool still prompts; dismissal denies.
    let returns = rt
        .dispatch(tool_call("edit_file", serde_json::json!({"path": "a.rs"})))
        .await
        .unwrap();
    assert!(
        matches!(&returns[..], [phi_ext::EventReturn::ToolBlock { reason }]
            if reason.contains("denied"))
    );
    host.await.unwrap();
}

#[tokio::test]
async fn defaults_approvals_command_lists_and_resets() {
    let dir = tempfile::tempdir().unwrap();
    let rt = approvals_runtime(dir.path(), serde_json::json!([]));
    let listing = rt
        .invoke_command_sync("approvals", "")
        .unwrap()
        .expect("registered")
        .message
        .unwrap();
    assert!(listing.contains("[default] ask repl *"));
    assert!(listing.contains("[default] ask bash *"));

    // Persist an answer, then reset clears it.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    rt.connect_ui(tx);
    let host = tokio::spawn(async move {
        let req = rx.recv().await.expect("prompt");
        req.reply.send(Some(1)).unwrap();
    });
    assert!(
        rt.dispatch(tool_call("bash", serde_json::json!({"cmd": "ls"})))
            .await
            .unwrap()
            .is_empty()
    );
    host.await.unwrap();
    let listing = rt
        .invoke_command_sync("approvals", "")
        .unwrap()
        .unwrap()
        .message
        .unwrap();
    assert!(listing.contains("[saved] allow bash *"));

    rt.invoke_command_sync("approvals", "reset").unwrap();
    let listing = rt
        .invoke_command_sync("approvals", "")
        .unwrap()
        .unwrap()
        .message
        .unwrap();
    assert!(!listing.contains("[saved]"));
    assert!(store_rules(dir.path()).is_empty());
}

/// Read-modify-write: a concurrent session's saved answers survive our
/// writes. The store is re-read at write time, not clobbered from the
/// startup snapshot.
#[tokio::test]
async fn defaults_approvals_store_writes_keep_other_projects() {
    let dir = tempfile::tempdir().unwrap();
    let rt = approvals_runtime(dir.path(), serde_json::json!([]));

    // After startup, another phi session persists an answer for a
    // different project into the same store file.
    std::fs::write(
        dir.path().join("approvals.json"),
        r#"{"/somewhere/else":[{"tool":"bash","pattern":"*","action":"allow"}]}"#,
    )
    .unwrap();

    // Our session persists an "always allow" answer.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    rt.connect_ui(tx);
    let host = tokio::spawn(async move {
        let req = rx.recv().await.expect("prompt");
        req.reply.send(Some(1)).unwrap();
    });
    assert!(
        rt.dispatch(tool_call("bash", serde_json::json!({"cmd": "ls"})))
            .await
            .unwrap()
            .is_empty()
    );
    host.await.unwrap();

    // Both projects' rules are in the store.
    let raw = std::fs::read_to_string(dir.path().join("approvals.json")).unwrap();
    let store: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert!(store.get("/somewhere/else").is_some(), "{store}");
    assert_eq!(store_rules(dir.path()).len(), 1);
}
