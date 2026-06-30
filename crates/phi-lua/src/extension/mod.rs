use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use phi_ext::{
    CommandInvocation, CommandOutput, CommandSpec, EventHandlerRegistration, ExtensionHost,
    ExtensionManifest, FlagSpec, FlagType, LifecycleEvent, MessageRendererSpec, OverlayOutcome,
    OverlaySpec, ProviderConfig, ProviderModelConfig, ShortcutSpec, SpinnerSpec, ThemeSpec,
    ThinkingExpressionSpec, ToolInvocation, ToolOutput, ToolParam, ToolSpec,
};
use std::collections::BTreeMap;

mod bridge;
mod convert;
mod event_return;
mod globals;
mod thread;

pub use thread::LuaMsg;

use event_return::{decode_event_return, parse_event_kind};
use globals::setup_phi_global;
use thread::spawn_lua_thread;

pub struct LuaExtension {
    path: PathBuf,
    source: String,
}

impl LuaExtension {
    pub fn from_file(path: &Path) -> Result<Self> {
        Ok(Self {
            path: path.to_owned(),
            source: std::fs::read_to_string(path)?,
        })
    }
}

impl phi_ext::Extension for LuaExtension {
    fn manifest(&self) -> ExtensionManifest {
        let stem = self
            .path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");
        ExtensionManifest {
            id: format!("lua:{stem}"),
            name: stem.to_owned(),
            version: "0.1.0".to_owned(),
            description: format!("Lua extension: {}", self.path.display()),
        }
    }

    fn register(&self, host: &mut dyn ExtensionHost) -> Result<()> {
        let lua = mlua::Lua::new();
        setup_phi_global(&lua)?;

        let filename = self
            .path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown.lua")
            .to_owned();

        lua.load(&self.source).set_name(&filename).exec()?;

        let globals = lua.globals();

        // ── Read registered events ────────────────────────────────────────────
        let reg_events: mlua::Table = globals.get("_phi_registered_events")?;
        let event_names: Vec<String> = (1..)
            .map_while(|i| reg_events.get::<Option<String>>(i).ok().flatten())
            .collect();

        // ── Read registered commands ──────────────────────────────────────────
        let reg_commands: mlua::Table = globals.get("_phi_registered_commands")?;
        let command_names: Vec<String> = (1..)
            .map_while(|i| reg_commands.get::<Option<String>>(i).ok().flatten())
            .collect();

        let phi_commands_table: mlua::Table = globals.get("_phi_commands")?;
        let mut commands: Vec<(String, String)> = Vec::new();
        for name in &command_names {
            let cmd_table: mlua::Table = phi_commands_table.get(name.as_str())?;
            let description: String = cmd_table.get("description").unwrap_or_default();
            commands.push((name.clone(), description));
        }

        // ── Read registered tools ────────────────────────────────────────────
        let reg_tools: mlua::Table = globals.get("_phi_registered_tools")?;
        let tool_names: Vec<String> = (1..)
            .map_while(|i| reg_tools.get::<Option<String>>(i).ok().flatten())
            .collect();

        let phi_tools_table: mlua::Table = globals.get("_phi_tools")?;
        let mut tools: Vec<(String, String, Vec<ToolParam>)> = Vec::new();
        for name in &tool_names {
            let tool_table: mlua::Table = phi_tools_table.get(name.as_str())?;
            let description: String = tool_table.get("description").unwrap_or_default();

            // Parse params: array of { name = "...", type = "..." [, default = "..." ] }
            let params_table: Option<mlua::Table> = tool_table.get("params").ok();
            let params = match params_table {
                Some(pt) => {
                    let mut result = Vec::new();
                    for pair in pt.pairs::<mlua::Value, mlua::Table>() {
                        let (_, p) = pair?;
                        let pname: String = p.get("name").unwrap_or_default();
                        let pty: String = p.get("type").unwrap_or_else(|_| "string".to_owned());
                        let pdefault: Option<String> = p.get("default").ok();
                        result.push(ToolParam {
                            name: pname,
                            ty: pty,
                            default: pdefault,
                        });
                    }
                    // Also handle array-style params (1-indexed table keys)
                    if result.is_empty() {
                        for pair in pt.pairs::<mlua::Value, mlua::Table>() {
                            let (_, p) = pair?;
                            let pname: String = p.get("name").unwrap_or_default();
                            let pty: String = p.get("type").unwrap_or_else(|_| "string".to_owned());
                            let pdefault: Option<String> = p.get("default").ok();
                            result.push(ToolParam {
                                name: pname,
                                ty: pty,
                                default: pdefault,
                            });
                        }
                    }
                    result
                }
                None => Vec::new(),
            };

            tools.push((name.clone(), description, params));
        }

        // ── Read registered shortcuts ────────────────────────────────────────
        let reg_shortcuts: mlua::Table = globals.get("_phi_registered_shortcuts")?;
        let shortcut_keys: Vec<String> = (1..)
            .map_while(|i| reg_shortcuts.get::<Option<String>>(i).ok().flatten())
            .collect();

        let phi_shortcuts_table: mlua::Table = globals.get("_phi_shortcuts")?;
        let mut shortcuts: Vec<(String, String)> = Vec::new();
        for key in &shortcut_keys {
            let sc_table: mlua::Table = phi_shortcuts_table.get(key.as_str())?;
            let description: String = sc_table.get("description").unwrap_or_default();
            shortcuts.push((key.clone(), description));
        }

        // ── Read registered flags ───────────────────────────────────────────
        let reg_flags: mlua::Table = globals.get("_phi_registered_flags")?;
        let flag_names: Vec<String> = (1..)
            .map_while(|i| reg_flags.get::<Option<String>>(i).ok().flatten())
            .collect();

        let phi_flags_table: mlua::Table = globals.get("_phi_flags")?;
        let mut flags: Vec<(String, String, FlagType, Option<String>)> = Vec::new();
        for name in &flag_names {
            let fl_table: mlua::Table = phi_flags_table.get(name.as_str())?;
            let description: String = fl_table.get("description").unwrap_or_default();
            let flag_type_str: String = fl_table
                .get::<String>("type")
                .unwrap_or_else(|_| "boolean".to_owned());
            let flag_type = if flag_type_str == "string" {
                FlagType::String
            } else {
                FlagType::Boolean
            };
            let default: Option<String> = fl_table.get("default").ok();
            flags.push((name.clone(), description, flag_type, default));
        }

        // ── Read registered providers ───────────────────────────────────────
        let reg_providers: mlua::Table = globals.get("_phi_registered_providers")?;
        let provider_names: Vec<String> = (1..)
            .map_while(|i| reg_providers.get::<Option<String>>(i).ok().flatten())
            .collect();

        let phi_providers_table: mlua::Table = globals.get("_phi_providers")?;
        let mut providers: Vec<ProviderConfig> = Vec::new();
        for name in &provider_names {
            let prov_table: mlua::Table = phi_providers_table.get(name.as_str())?;
            let base_url: Option<String> = prov_table.get("base_url").ok();
            let api_key: Option<String> = prov_table.get("api_key").ok();
            let api: Option<String> = prov_table.get("api").ok();

            let mut models: Vec<ProviderModelConfig> = Vec::new();
            let models_table: Option<mlua::Table> = prov_table.get("models").ok();
            if let Some(mt) = models_table {
                for pair in mt.pairs::<mlua::Value, mlua::Table>() {
                    let (_, m) = pair?;
                    let id: String = m.get("id").unwrap_or_default();
                    let mname: String = m.get("name").unwrap_or_default();
                    let reasoning: bool = m.get("reasoning").unwrap_or(false);
                    let context_window: u64 = m.get("context_window").unwrap_or(0);
                    let max_tokens: u64 = m.get("max_tokens").unwrap_or(0);
                    models.push(ProviderModelConfig {
                        id,
                        name: mname,
                        reasoning,
                        context_window,
                        max_tokens,
                    });
                }
            }

            providers.push(ProviderConfig {
                name: name.clone(),
                base_url,
                api_key,
                api,
                models,
            });
        }

        // ── Read registered message renderers ───────────────────────────────
        let reg_renderers: mlua::Table = globals.get("_phi_registered_renderers")?;
        let renderer_types: Vec<String> = (1..)
            .map_while(|i| reg_renderers.get::<Option<String>>(i).ok().flatten())
            .collect();

        // ── Read registered themes ───────────────────────────────────────────
        let reg_themes: mlua::Table = globals.get("_phi_registered_themes")?;
        let theme_names: Vec<String> = (1..)
            .map_while(|i| reg_themes.get::<Option<String>>(i).ok().flatten())
            .collect();

        let phi_themes_table: mlua::Table = globals.get("_phi_themes")?;
        let mut themes: Vec<ThemeSpec> = Vec::new();
        for name in &theme_names {
            let theme_table: mlua::Table = phi_themes_table.get(name.as_str())?;
            let mut palette = BTreeMap::new();
            for pair in theme_table.pairs::<String, mlua::Table>() {
                let (role, rgb_table) = pair?;
                let r: u8 = rgb_table.get(1).unwrap_or(0);
                let g: u8 = rgb_table.get(2).unwrap_or(0);
                let b: u8 = rgb_table.get(3).unwrap_or(0);
                palette.insert(role, phi_ext::Rgb(r, g, b));
            }
            themes.push(ThemeSpec::new(name.clone(), palette));
        }

        // ── Read registered spinners ───────────────────────────────────────
        let reg_spinners: mlua::Table = globals.get("_phi_registered_spinners")?;
        let spinner_names: Vec<String> = (1..)
            .map_while(|i| reg_spinners.get::<Option<String>>(i).ok().flatten())
            .collect();

        let phi_spinners_table: mlua::Table = globals.get("_phi_spinners")?;
        let mut spinners: Vec<SpinnerSpec> = Vec::new();
        for name in &spinner_names {
            let sp_table: mlua::Table = phi_spinners_table.get(name.as_str())?;
            let frame_interval_ms: u64 = sp_table.get("frame_interval_ms").unwrap_or(80);
            let frames_table: mlua::Table = sp_table
                .get("frames")
                .unwrap_or_else(|_| lua.create_table().unwrap());
            let mut frames: Vec<String> = Vec::new();
            for pair in frames_table.pairs::<mlua::Value, String>() {
                let (_, f) = pair?;
                frames.push(f);
            }
            // Also handle array-style (1-indexed)
            if frames.is_empty() {
                for i in 1..=frames_table.raw_len() {
                    if let Ok(f) = frames_table.get::<String>(i) {
                        frames.push(f);
                    }
                }
            }
            spinners.push(SpinnerSpec::new(name.clone(), frames, frame_interval_ms));
        }

        // ── Read registered thinking expressions ───────────────────────────
        let reg_thinking: mlua::Table = globals.get("_phi_registered_thinking_expressions")?;
        let thinking_names: Vec<String> = (1..)
            .map_while(|i| reg_thinking.get::<Option<String>>(i).ok().flatten())
            .collect();

        let phi_thinking_table: mlua::Table = globals.get("_phi_thinking_expressions")?;
        let mut thinking_exprs: Vec<ThinkingExpressionSpec> = Vec::new();
        for name in &thinking_names {
            let te_table: mlua::Table = phi_thinking_table.get(name.as_str())?;
            let tool_expression: String = te_table
                .get("tool_expression")
                .unwrap_or_else(|_| "working...".to_owned());
            let expression_interval_ms: u64 =
                te_table.get("expression_interval_ms").unwrap_or(2000);
            let exprs_table: mlua::Table = te_table
                .get("expressions")
                .unwrap_or_else(|_| lua.create_table().unwrap());
            let mut expressions: Vec<String> = Vec::new();
            for pair in exprs_table.pairs::<mlua::Value, String>() {
                let (_, e) = pair?;
                expressions.push(e);
            }
            // Also handle array-style
            if expressions.is_empty() {
                for i in 1..=exprs_table.raw_len() {
                    if let Ok(e) = exprs_table.get::<String>(i) {
                        expressions.push(e);
                    }
                }
            }
            thinking_exprs.push(ThinkingExpressionSpec::new(
                name.clone(),
                expressions,
                tool_expression,
                expression_interval_ms,
            ));
        }

        // ── Read registered overlays ──────────────────────────────────────────
        let reg_overlays: mlua::Table = globals.get("_phi_registered_overlays")?;
        let overlay_names: Vec<String> = (1..)
            .map_while(|i| reg_overlays.get::<Option<String>>(i).ok().flatten())
            .collect();

        let phi_overlays_table: mlua::Table = globals.get("_phi_overlays")?;
        let mut lua_overlays: Vec<(String, String, String)> = Vec::new();
        for name in &overlay_names {
            let ov_table: mlua::Table = phi_overlays_table.get(name.as_str())?;
            let description: String = ov_table.get("description").unwrap_or_default();
            let toggle_key: String = ov_table.get("toggle_key").unwrap_or_else(|_| "".to_owned());
            lua_overlays.push((name.clone(), description, toggle_key));
        }

        // ── Create the channel and spawn the Lua thread ─────────────────────
        let (tx, rx) = std::sync::mpsc::sync_channel::<LuaMsg>(32);
        spawn_lua_thread(lua, rx);
        let tx = Arc::new(tx);

        // ── Register event handlers ──────────────────────────────────────────
        for event_name in event_names {
            let kind = parse_event_kind(&event_name)?;
            let tx2 = Arc::clone(&tx);
            let name2 = event_name;
            host.subscribe(EventHandlerRegistration {
                event: kind,
                label: format!("lua:{}:{}", self.path.display(), &name2),
                handler: Arc::new(move |event: LifecycleEvent| {
                    let payload_json = serde_json::to_string(&event.payload)?;
                    let (rtx, rrx) = std::sync::mpsc::sync_channel(1);
                    tx2.send(LuaMsg::CallEvent {
                        event_name: name2.clone(),
                        payload_json,
                        reply: rtx,
                    })
                    .map_err(|e| anyhow::anyhow!("lua send: {e}"))?;
                    let json_opt = rrx
                        .recv_timeout(Duration::from_secs(10))
                        .map_err(|e| anyhow::anyhow!("lua recv: {e}"))??;
                    decode_event_return(json_opt)
                }),
            })?;
        }

        // ── Register commands ──────────────────────────────────────────────
        for (cmd_name, description) in commands {
            let tx3 = Arc::clone(&tx);
            let name3 = cmd_name.clone();
            host.register_command(CommandSpec {
                name: cmd_name,
                description,
                handler: Arc::new(move |inv: CommandInvocation| {
                    let (rtx, rrx) = std::sync::mpsc::sync_channel(1);
                    tx3.send(LuaMsg::InvokeCommand {
                        name: name3.clone(),
                        args: inv.raw_args,
                        reply: rtx,
                    })
                    .map_err(|e| anyhow::anyhow!("lua send: {e}"))?;
                    let msg_opt = rrx
                        .recv_timeout(Duration::from_secs(10))
                        .map_err(|e| anyhow::anyhow!("lua recv: {e}"))??;
                    Ok(CommandOutput { message: msg_opt })
                }),
            })?;
        }

        // ── Register tools ──────────────────────────────────────────────────
        for (tool_name, description, params) in tools {
            let tx4 = Arc::clone(&tx);
            let name4 = tool_name.clone();
            host.register_tool(ToolSpec {
                name: tool_name,
                description,
                params,
                handler: Arc::new(move |inv: ToolInvocation| {
                    let (rtx, rrx) = std::sync::mpsc::sync_channel(1);
                    tx4.send(LuaMsg::InvokeTool {
                        name: name4.clone(),
                        input: inv.input,
                        reply: rtx,
                    })
                    .map_err(|e| anyhow::anyhow!("lua send: {e}"))?;
                    let content_opt = rrx
                        .recv_timeout(Duration::from_secs(10))
                        .map_err(|e| anyhow::anyhow!("lua recv: {e}"))??;
                    Ok(ToolOutput {
                        content: content_opt.unwrap_or_default(),
                    })
                }),
            })?;
        }

        // ── Register shortcuts ──────────────────────────────────────────────
        for (key, description) in shortcuts {
            host.register_shortcut(ShortcutSpec {
                key,
                description,
                // Shortcuts are handled at the TUI layer; the handler is a no-op stub here.
                handler: Arc::new(|_ctx| Ok(())),
            })?;
        }

        // ── Register flags ──────────────────────────────────────────────────
        for (name, description, flag_type, default) in flags {
            host.register_flag(FlagSpec {
                name,
                description,
                flag_type,
                default,
            })?;
        }

        // ── Register providers ───────────────────────────────────────────────
        for config in providers {
            host.register_provider(config)?;
        }

        // ── Register message renderers ──────────────────────────────────────
        for custom_type in renderer_types {
            let tx5 = Arc::clone(&tx);
            let ct = custom_type.clone();
            host.register_message_renderer(MessageRendererSpec {
                custom_type: custom_type.clone(),
                handler: Arc::new(move |message: serde_json::Value| {
                    // Serialize the message to JSON, send it to Lua for rendering
                    let message_json = serde_json::to_string(&message)?;
                    let (rtx, rrx) = std::sync::mpsc::sync_channel(1);
                    tx5.send(LuaMsg::CallEvent {
                        event_name: format!("__render_{}", ct),
                        payload_json: message_json,
                        reply: rtx,
                    })
                    .map_err(|e| anyhow::anyhow!("lua send: {e}"))?;
                    let result = rrx
                        .recv_timeout(Duration::from_secs(10))
                        .map_err(|e| anyhow::anyhow!("lua recv: {e}"))??;
                    Ok(result.unwrap_or_default())
                }),
            })?;
        }

        // ── Register themes ──────────────────────────────────────────────────
        for theme in themes {
            host.register_theme(theme)?;
        }

        // ── Register spinners ───────────────────────────────────────────────
        for spinner in spinners {
            host.register_spinner(spinner)?;
        }

        // ── Register thinking expressions ───────────────────────────────────
        for expr in thinking_exprs {
            host.register_thinking_expression(expr)?;
        }

        // ── Register overlays ──────────────────────────────────────────────
        for (name, description, toggle_key) in lua_overlays {
            let tx_overlay = Arc::clone(&tx);
            let overlay_name = name.clone();

            let init_state: Arc<dyn Fn() -> Box<dyn std::any::Any + Send + Sync> + Send + Sync> =
                Arc::new(|| Box::new(Vec::<String>::new()) as Box<dyn std::any::Any + Send + Sync>);

            let render: Arc<
                dyn Fn(&mut dyn phi_ext::DrawContext, &mut Box<dyn std::any::Any + Send + Sync>)
                    + Send
                    + Sync,
            > = Arc::new(
                move |ctx: &mut dyn phi_ext::DrawContext,
                      _state: &mut Box<dyn std::any::Any + Send + Sync>| {
                    // Call the Lua render function via the thread channel
                    let (rtx, rrx) = std::sync::mpsc::sync_channel(1);
                    if tx_overlay
                        .send(crate::extension::LuaMsg::RenderOverlay {
                            name: overlay_name.clone(),
                            reply: rtx,
                        })
                        .is_err()
                    {
                        return;
                    }
                    let lines: Vec<String> = match rrx.recv_timeout(Duration::from_secs(5)) {
                        Ok(Ok(Some(json))) => serde_json::from_str(&json).unwrap_or_default(),
                        _ => vec!["(render failed)".to_owned()],
                    };

                    // Draw the lines in a bordered box
                    let size = ctx.size();
                    let line_count = lines.len() as i32 + 4; // border + title + padding
                    let width = size.cols.min(80).max(20);
                    let height = size.rows.min(line_count).max(6);
                    let col = (size.cols - width) / 2;
                    let row = (size.rows - height) / 2;
                    let rect = phi_ext::DrawRect::new(col, row, width, height);

                    let border = phi_ext::Rgb(100, 100, 100);
                    let fg = phi_ext::Rgb(220, 220, 220);
                    let bg = phi_ext::Rgb(30, 30, 30);
                    let accent = phi_ext::Rgb(100, 200, 255);

                    ctx.draw_box(rect, fg, bg, border);
                    let inner = rect.inset(2, 1);

                    // Title
                    ctx.put_text_bold(
                        inner.col,
                        inner.row,
                        inner.cols,
                        &format!(" {} ", "Overlay"),
                        accent,
                        bg,
                    );

                    // Lines
                    for (i, line) in lines.iter().enumerate() {
                        let y = inner.row + 2 + i as i32;
                        if y < rect.row + rect.rows - 2 {
                            ctx.put_text(inner.col, y, inner.cols, line, fg, bg);
                        }
                    }

                    // Footer hint
                    let footer = rect.row + rect.rows - 2;
                    ctx.put_text(
                        inner.col,
                        footer,
                        inner.cols,
                        "Esc/Enter/q to close",
                        phi_ext::Rgb(120, 120, 120),
                        bg,
                    );
                },
            );

            let handle_key: Arc<
                dyn Fn(
                        &mut Box<dyn std::any::Any + Send + Sync>,
                        &phi_ext::OverlayKey,
                        u16,
                        u16,
                    ) -> OverlayOutcome
                    + Send
                    + Sync,
            > = Arc::new(
                |_state: &mut Box<dyn std::any::Any + Send + Sync>,
                 key: &phi_ext::OverlayKey,
                 _cols: u16,
                 _rows: u16| {
                    use phi_ext::OverlayKeyCode;
                    match key.code {
                        OverlayKeyCode::Esc | OverlayKeyCode::Enter => OverlayOutcome::Close,
                        OverlayKeyCode::Char('q') | OverlayKeyCode::Char('Q')
                            if !key.modifiers.control =>
                        {
                            OverlayOutcome::Close
                        }
                        _ => OverlayOutcome::None,
                    }
                },
            );

            host.register_overlay(OverlaySpec::new(
                name,
                description,
                toggle_key,
                init_state,
                render,
                handle_key,
            ))?;
        }

        Ok(())
    }
}
