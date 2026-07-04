use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use phi_ext::{
    CommandInvocation, CommandOutput, CommandSpec, EventHandlerRegistration, ExtensionHost,
    ExtensionManifest, FlagSpec, FlagType, LifecycleEvent, MessageRendererSpec, ProviderConfig,
    ProviderModelConfig, ShortcutSpec, SpinnerSpec, StyleSpec, ThemeSpec, ThinkingExpressionSpec,
    ToolDispatch, ToolExposure, ToolInvocation, ToolOutput, ToolParam, ToolRenderSpec, ToolSpec,
};
use std::collections::BTreeMap;

mod bridge;
mod convert;
mod event_return;
mod globals;
mod overlay;
mod thread;

pub use thread::LuaMsg;

use convert::parse_command_output;
use event_return::{decode_event_return, parse_event_kind};
use globals::setup_phi_global;
use thread::spawn_lua_thread;

pub struct LuaExtension {
    path: PathBuf,
    source: String,
    /// Host-injected globals, set on the VM (via typed JSON→Lua conversion)
    /// before the source runs. This is the host→extension config channel:
    /// no hand-serialized Lua source, no escaping bugs.
    globals: Vec<(String, serde_json::Value)>,
}

impl LuaExtension {
    pub fn from_file(path: &Path) -> Result<Self> {
        Ok(Self {
            path: path.to_owned(),
            source: std::fs::read_to_string(path)?,
            globals: Vec::new(),
        })
    }

    /// Build a Lua extension from inline source. Used by `phi-builtins` to
    /// ship default extensions as embedded Lua (`include_str!`) — the
    /// defaults are the same `.lua` a user would write, compiled into the
    /// binary for zero-startup-cost, readable in the repo for zero-cost
    /// learning.
    ///
    /// `name` becomes the manifest id (`lua:{name}`) and the Lua chunk name
    /// for error messages; it does not need a file on disk.
    pub fn from_source(name: &str, source: String) -> Result<Self> {
        Ok(Self {
            path: PathBuf::from(format!("{name}.lua")),
            source,
            globals: Vec::new(),
        })
    }

    /// Inject a global into the VM before the source runs. The value is a
    /// JSON value converted with the same semantics as `phi.json.decode`
    /// (JSON `null` → Lua `nil`), so `_phi_*` runtime globals reach the
    /// extension as real tables — never as concatenated source text.
    pub fn with_global(mut self, name: &str, value: serde_json::Value) -> Self {
        self.globals.push((name.to_owned(), value));
        self
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
        // `phi.ui` (ask-the-user) is late-bound: the handle is disconnected
        // until an interactive host calls `connect_ui`, and `phi.ui.ask`
        // resolves to `nil, "no_ui"` everywhere else.
        globals::attach_ui_namespace(&lua, host.ui_handle())?;

        let filename = self
            .path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown.lua")
            .to_owned();

        // Host-injected globals land before the source runs.
        for (name, value) in &self.globals {
            lua.globals()
                .set(name.as_str(), globals::json_to_lua(&lua, value)?)?;
        }

        lua.load(&self.source).set_name(&filename).exec()?;

        let globals = lua.globals();

        // ── Read registered events ────────────────────────────────────────────
        let reg_events: mlua::Table = globals.get("_phi_registered_events")?;
        let event_names: Vec<String> = (1..)
            .map_while(|i| reg_events.get::<Option<String>>(i).ok().flatten())
            .collect();
        // Per-event `phi.on` opts: `timeout` (seconds) overrides the dispatch
        // watchdog budget (see EventHandlerRegistration::timeout).
        let event_opts: mlua::Table = globals.get("_phi_event_opts")?;
        let event_timeouts: BTreeMap<String, Duration> = event_names
            .iter()
            .filter_map(|name| {
                let secs = event_opts
                    .get::<Option<mlua::Table>>(name.as_str())
                    .ok()
                    .flatten()?
                    .get::<Option<f64>>("timeout")
                    .ok()
                    .flatten()?;
                Some((name.clone(), Duration::from_secs_f64(secs.max(0.0))))
            })
            .collect();

        // ── Read registered commands ──────────────────────────────────────────
        let reg_commands: mlua::Table = globals.get("_phi_registered_commands")?;
        let command_names: Vec<String> = (1..)
            .map_while(|i| reg_commands.get::<Option<String>>(i).ok().flatten())
            .collect();

        /// One command registration read from Lua:
        /// (name, description, args_hint, context, aliases, completions).
        type LuaCommandReg = (
            String,
            String,
            String,
            phi_ext::CommandContext,
            Vec<String>,
            Option<Vec<String>>,
        );
        let phi_commands_table: mlua::Table = globals.get("_phi_commands")?;
        let mut commands: Vec<LuaCommandReg> = Vec::new();
        for name in &command_names {
            let cmd_table: mlua::Table = phi_commands_table.get(name.as_str())?;
            let description: String = cmd_table.get("description").unwrap_or_default();
            let args_hint: String = cmd_table.get("args_hint").unwrap_or_default();
            // Context tag: "session" (default) or "mux" — same vocabulary
            // the builtin registrations use (WS1.7).
            let context = match cmd_table
                .get::<String>("context")
                .unwrap_or_default()
                .as_str()
            {
                "mux" => phi_ext::CommandContext::Mux,
                _ => phi_ext::CommandContext::Session,
            };
            let aliases: Vec<String> = match cmd_table.get::<Option<mlua::Table>>("aliases") {
                Ok(Some(t)) => (1..=t.raw_len())
                    .filter_map(|i| t.get::<String>(i).ok())
                    .collect(),
                _ => Vec::new(),
            };
            // Static completion candidates for the first argument.
            let completions: Option<Vec<String>> =
                match cmd_table.get::<Option<mlua::Table>>("completions") {
                    Ok(Some(t)) => Some(
                        (1..=t.raw_len())
                            .filter_map(|i| t.get::<String>(i).ok())
                            .collect(),
                    ),
                    _ => None,
                };
            commands.push((
                name.clone(),
                description,
                args_hint,
                context,
                aliases,
                completions,
            ));
        }

        // ── Read registered tools ────────────────────────────────────────────
        let reg_tools: mlua::Table = globals.get("_phi_registered_tools")?;
        let tool_names: Vec<String> = (1..)
            .map_while(|i| reg_tools.get::<Option<String>>(i).ok().flatten())
            .collect();

        /// One tool registration read from Lua:
        /// (name, description, params, expose, input_schema, dispatch).
        type LuaToolReg = (
            String,
            String,
            Vec<ToolParam>,
            ToolExposure,
            Option<serde_json::Value>,
            ToolDispatch,
        );
        let phi_tools_table: mlua::Table = globals.get("_phi_tools")?;
        let mut tools: Vec<LuaToolReg> = Vec::new();
        for name in &tool_names {
            let tool_table: mlua::Table = phi_tools_table.get(name.as_str())?;
            let description: String = tool_table.get("description").unwrap_or_default();

            // Exposure: "repl" (default) | "native" | "both".
            let expose = match tool_table
                .get::<String>("expose")
                .unwrap_or_default()
                .as_str()
            {
                "native" => ToolExposure::Native,
                "both" => ToolExposure::Both,
                _ => ToolExposure::Repl,
            };

            // Optional explicit JSON Schema for the native definition;
            // derived from params when absent.
            let input_schema: Option<serde_json::Value> = match tool_table
                .get::<Option<mlua::Table>>("input_schema")
                .unwrap_or_default()
            {
                Some(t) => {
                    use mlua::LuaSerdeExt as _;
                    Some(lua.from_value(mlua::Value::Table(t)).map_err(|e| {
                        mlua::Error::runtime(format!("tool '{name}': bad input_schema: {e}"))
                    })?)
                }
                None => None,
            };

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

            // Dispatch: "python" dispatches into the REPL env's prelude
            // functions via `__phi_native_call__` (no Lua handler needed);
            // default is a Handler that round-trips through the Lua thread.
            let dispatch = match tool_table
                .get::<String>("dispatch")
                .unwrap_or_default()
                .as_str()
            {
                "python" => ToolDispatch::Python,
                _ => ToolDispatch::Handler(Arc::new(|_inv: ToolInvocation| {
                    Err(anyhow::anyhow!(
                        "tool handler not set (no `handler` function)"
                    ))
                })),
            };
            tools.push((
                name.clone(),
                description,
                params,
                expose,
                input_schema,
                dispatch,
            ));
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

            // Model tables deserialize straight into `ProviderModelConfig`,
            // so Lua extensions get every field the config schema has (cost,
            // thinking_levels, headers, ...) without hand-mirrored reads.
            let mut models: Vec<ProviderModelConfig> = Vec::new();
            let models_table: Option<mlua::Table> = prov_table.get("models").ok();
            if let Some(mt) = models_table {
                use mlua::LuaSerdeExt as _;
                for pair in mt.pairs::<mlua::Value, mlua::Value>() {
                    let (_, m) = pair?;
                    let model: ProviderModelConfig = lua.from_value(m).map_err(|e| {
                        mlua::Error::runtime(format!("provider '{name}': bad model config: {e}"))
                    })?;
                    models.push(model);
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

        // ── Read registered tool renderers ──────────────────────────────────
        let reg_tool_renderers: mlua::Table = globals.get("_phi_registered_tool_renderers")?;
        let tool_renderer_names: Vec<String> = (1..)
            .map_while(|i| reg_tool_renderers.get::<Option<String>>(i).ok().flatten())
            .collect();

        let phi_tool_renderers_table: mlua::Table = globals.get("_phi_tool_renderers")?;
        let mut tool_renderers: Vec<ToolRenderSpec> = Vec::new();
        for name in &tool_renderer_names {
            let spec_table: mlua::Table = phi_tool_renderers_table.get(name.as_str())?;
            tool_renderers.push(tool_render_spec_from_table(&lua, name, spec_table)?);
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
            themes.push(ThemeSpec::new(
                name.clone(),
                palette_from_table(&theme_table)?,
            ));
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
            spinners.push(spinner_from_table(&lua, name.clone(), &sp_table)?);
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
            thinking_exprs.push(thinking_from_table(&lua, name.clone(), &te_table)?);
        }

        // ── Read registered styles (WS10) ──────────────────────────────────
        // A style bundles the shapes parsed above; each member reuses the
        // same parser as its standalone registration, so the vocabulary is
        // identical in and out of a bundle.
        let reg_styles: mlua::Table = globals.get("_phi_registered_styles")?;
        let style_names: Vec<String> = (1..)
            .map_while(|i| reg_styles.get::<Option<String>>(i).ok().flatten())
            .collect();

        let phi_styles_table: mlua::Table = globals.get("_phi_styles")?;
        let mut styles: Vec<StyleSpec> = Vec::new();
        for name in &style_names {
            let st: mlua::Table = phi_styles_table.get(name.as_str())?;
            let mut spec = StyleSpec::new(name.clone());
            if let Some(theme_table) = st.get::<Option<mlua::Table>>("theme")? {
                spec.theme = Some(ThemeSpec::new(
                    format!("{name}:theme"),
                    palette_from_table(&theme_table)?,
                ));
            }
            if let Some(sp_table) = st.get::<Option<mlua::Table>>("spinner")? {
                spec.spinner = Some(spinner_from_table(
                    &lua,
                    format!("{name}:spinner"),
                    &sp_table,
                )?);
            }
            if let Some(te_table) = st.get::<Option<mlua::Table>>("thinking")? {
                spec.thinking_expression = Some(thinking_from_table(
                    &lua,
                    format!("{name}:thinking"),
                    &te_table,
                )?);
            }
            if let Some(trs) = st.get::<Option<mlua::Table>>("tool_renderers")? {
                for pair in trs.pairs::<String, mlua::Table>() {
                    let (tool, spec_table) = pair?;
                    spec.tool_renderers
                        .push(tool_render_spec_from_table(&lua, &tool, spec_table)?);
                }
            }
            if let Some(ch_table) = st.get::<Option<mlua::Table>>("chrome")? {
                spec.chrome = Some(chrome_spec_from_table(&lua, name, ch_table)?);
            }
            styles.push(spec);
        }

        // ── Read registered overlays ──────────────────────────────────────────
        let reg_overlays: mlua::Table = globals.get("_phi_registered_overlays")?;
        let overlay_names: Vec<String> = (1..)
            .map_while(|i| reg_overlays.get::<Option<String>>(i).ok().flatten())
            .collect();

        let phi_overlays_table: mlua::Table = globals.get("_phi_overlays")?;
        let mut lua_overlays: Vec<(String, String, String, overlay::LuaOverlayFns)> = Vec::new();
        for name in &overlay_names {
            let ov_table: mlua::Table = phi_overlays_table.get(name.as_str())?;
            let description: String = ov_table.get("description").unwrap_or_default();
            let toggle_key: String = ov_table.get("toggle_key").unwrap_or_else(|_| "".to_owned());
            // Which optional hooks the registration defined — read once so
            // absent hooks get host-side fallbacks without a round trip.
            let has = |key: &str| {
                ov_table
                    .get::<Option<mlua::Function>>(key)
                    .ok()
                    .flatten()
                    .is_some()
            };
            let fns = overlay::LuaOverlayFns {
                has_handle_key: has("handle_key"),
                has_handle_scroll: has("handle_scroll"),
            };
            lua_overlays.push((name.clone(), description, toggle_key, fns));
        }

        // ── Create the channel and spawn the Lua thread ─────────────────────
        let (tx, rx) = std::sync::mpsc::sync_channel::<LuaMsg>(32);
        spawn_lua_thread(lua, rx);
        let tx = Arc::new(tx);

        // ── Register event handlers ──────────────────────────────────────────
        for event_name in event_names {
            let kind = parse_event_kind(&event_name)?;
            let tx2 = Arc::clone(&tx);
            let timeout = event_timeouts.get(&event_name).copied();
            // The bridge's own wait on the Lua thread tracks the registration
            // timeout (default 10s): a handler blocking on phi.ui.ask must
            // not be cut off by the bridge before the dispatch watchdog —
            // whichever budget the registration declared — has its say.
            let bridge_wait = timeout.unwrap_or(Duration::from_secs(10));
            let name2 = event_name;
            host.subscribe(EventHandlerRegistration {
                timeout,
                event: kind,
                label: format!("lua:{}:{}", self.path.display(), &name2),
                handler: Arc::new(move |event: LifecycleEvent| {
                    let payload_json = serde_json::to_string(&event.payload)?;
                    let (rtx, rrx) = std::sync::mpsc::sync_channel(1);
                    tx2.send(LuaMsg::CallEvent {
                        event_name: name2.clone(),
                        payload_json,
                        timeout_ms: timeout.map(|t| t.as_millis() as i64),
                        reply: rtx,
                    })
                    .map_err(|e| anyhow::anyhow!("lua send: {e}"))?;
                    let json_opt = rrx
                        .recv_timeout(bridge_wait)
                        .map_err(|e| anyhow::anyhow!("lua recv: {e}"))??;
                    decode_event_return(json_opt)
                }),
            })?;
        }

        // ── Register commands ──────────────────────────────────────────────
        for (cmd_name, description, args_hint, context, aliases, completions) in commands {
            let tx3 = Arc::clone(&tx);
            let name3 = cmd_name.clone();
            host.register_command(CommandSpec {
                name: cmd_name,
                description,
                args_hint,
                handler: Arc::new(move |inv: CommandInvocation| {
                    let (rtx, rrx) = std::sync::mpsc::sync_channel(1);
                    tx3.send(LuaMsg::InvokeCommand {
                        name: name3.clone(),
                        args: inv.raw_args,
                        reply: rtx,
                    })
                    .map_err(|e| anyhow::anyhow!("lua send: {e}"))?;
                    let output_json = rrx
                        .recv_timeout(Duration::from_secs(10))
                        .map_err(|e| anyhow::anyhow!("lua recv: {e}"))??;
                    // Full CommandOutput vocabulary: message + UiActions —
                    // the same contract builtin command handlers return.
                    match output_json {
                        Some(json) => parse_command_output(&json),
                        None => Ok(CommandOutput::default()),
                    }
                }),
                arg_completions: completions.map(|candidates| {
                    let candidates = Arc::new(candidates);
                    Arc::new(move || candidates.as_ref().clone()) as phi_ext::ArgCompletionFn
                }),
                context,
                aliases,
            })?;
        }

        // ── Register tools ──────────────────────────────────────────────────
        for (tool_name, description, params, expose, input_schema, dispatch) in tools {
            // When dispatch is "python", the tool is implemented by a same-named
            // prelude function — no Lua handler needed. Otherwise, a handler
            // round-trips through the Lua thread.
            let dispatch = match dispatch {
                ToolDispatch::Python => ToolDispatch::Python,
                _ => {
                    let tx4 = Arc::clone(&tx);
                    let name4 = tool_name.clone();
                    ToolDispatch::Handler(Arc::new(move |inv: ToolInvocation| {
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
                    }))
                }
            };
            host.register_tool(ToolSpec {
                name: tool_name,
                description,
                params,
                expose,
                input_schema,
                dispatch,
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

        // ── Register tool renderers ─────────────────────────────────────────
        for spec in tool_renderers {
            host.register_tool_renderer(spec)?;
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
                        timeout_ms: None,
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

        // ── Register styles ─────────────────────────────────────────────────
        for style in styles {
            host.register_style(style)?;
        }

        // ── Register overlays ──────────────────────────────────────────────
        for (name, description, toggle_key, fns) in lua_overlays {
            host.register_overlay(overlay::build_overlay_spec(
                name,
                description,
                toggle_key,
                fns,
                Arc::clone(&tx),
            ))?;
        }

        Ok(())
    }
}

// ── Visual table parsers ────────────────────────────────────────────────
// Shared by the standalone registrations (`phi.register_theme`/`_spinner`/
// `_thinking_expression`/`_tool_renderer`) and style-pack members
// (`phi.register_style`), so a shape means the same thing in and out of a
// bundle.

/// Parse a `{ role = {r,g,b}, ... }` palette table.
fn palette_from_table(theme_table: &mlua::Table) -> mlua::Result<BTreeMap<String, phi_ext::Rgb>> {
    let mut palette = BTreeMap::new();
    for pair in theme_table.pairs::<String, mlua::Table>() {
        let (role, rgb_table) = pair?;
        let r: u8 = rgb_table.get(1).unwrap_or(0);
        let g: u8 = rgb_table.get(2).unwrap_or(0);
        let b: u8 = rgb_table.get(3).unwrap_or(0);
        palette.insert(role, phi_ext::Rgb(r, g, b));
    }
    Ok(palette)
}

/// Parse a `{ frames = {...}, frame_interval_ms = 80 }` spinner table.
fn spinner_from_table(
    lua: &mlua::Lua,
    name: String,
    sp_table: &mlua::Table,
) -> mlua::Result<SpinnerSpec> {
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
    Ok(SpinnerSpec::new(name, frames, frame_interval_ms))
}

/// Parse a `{ expressions = {...}, tool_expression = "...",
/// expression_interval_ms = 2000 }` thinking-expression table.
fn thinking_from_table(
    lua: &mlua::Lua,
    name: String,
    te_table: &mlua::Table,
) -> mlua::Result<ThinkingExpressionSpec> {
    let tool_expression: String = te_table
        .get("tool_expression")
        .unwrap_or_else(|_| "working...".to_owned());
    let expression_interval_ms: u64 = te_table.get("expression_interval_ms").unwrap_or(2000);
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
    // Selection mode: `"cycle"` (default, timer-cycled) or `"per_turn"`
    // (one expression held for the whole turn, claude-code style).
    let selection = match te_table
        .get::<Option<String>>("selection")
        .unwrap_or_default()
        .as_deref()
    {
        None | Some("cycle") => phi_ext::ExpressionSelection::Cycle,
        Some("per_turn") => phi_ext::ExpressionSelection::PerTurn,
        Some(other) => {
            return Err(mlua::Error::runtime(format!(
                "thinking expression '{name}': bad selection '{other}' (expected \"cycle\" or \"per_turn\")"
            )));
        }
    };
    Ok(
        ThinkingExpressionSpec::new(name, expressions, tool_expression, expression_interval_ms)
            .with_selection(selection),
    )
}

/// Deserialize a style's `chrome` member straight into `ChromeSpec` —
/// omitted fields keep Phi's stock look via the spec's serde defaults.
/// Malformed tables are hard, named errors.
fn chrome_spec_from_table(
    lua: &mlua::Lua,
    style: &str,
    chrome_table: mlua::Table,
) -> mlua::Result<phi_ext::ChromeSpec> {
    use mlua::LuaSerdeExt as _;
    lua.from_value(mlua::Value::Table(chrome_table))
        .map_err(|e| mlua::Error::runtime(format!("style '{style}': bad chrome: {e}")))
}

/// Deserialize a tool renderer spec table. The tool name is the
/// registration key, not a table field; spec tables deserialize straight
/// into `ToolRenderSpec` so Lua gets the full declarative vocabulary
/// without hand-mirrored reads. Malformed specs are hard, named errors.
fn tool_render_spec_from_table(
    lua: &mlua::Lua,
    name: &str,
    spec_table: mlua::Table,
) -> mlua::Result<ToolRenderSpec> {
    use mlua::LuaSerdeExt as _;
    spec_table.set("tool", name)?;
    lua.from_value(mlua::Value::Table(spec_table))
        .map_err(|e| mlua::Error::runtime(format!("tool renderer '{name}': bad spec: {e}")))
}
