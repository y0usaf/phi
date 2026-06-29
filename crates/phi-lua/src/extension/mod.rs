use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use phi_ext::{
    CommandInvocation, CommandOutput, CommandSpec, EventHandlerRegistration, ExtensionHost,
    ExtensionManifest, FlagSpec, FlagType, LifecycleEvent, MessageRendererSpec,
    ProviderConfig, ProviderModelConfig, ShortcutSpec, ToolInvocation, ToolOutput, ToolParam,
    ToolSpec,
};

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
                    let json_opt = rrx.recv().map_err(|e| anyhow::anyhow!("lua recv: {e}"))??;
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
                    let msg_opt = rrx.recv().map_err(|e| anyhow::anyhow!("lua recv: {e}"))??;
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
                    let content_opt = rrx.recv().map_err(|e| anyhow::anyhow!("lua recv: {e}"))??;
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
                    let result = rrx.recv().map_err(|e| anyhow::anyhow!("lua recv: {e}"))??;
                    Ok(result.unwrap_or_default())
                }),
            })?;
        }

        Ok(())
    }
}
