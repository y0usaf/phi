/// Convert a JSON value to a Lua value with extension-friendly semantics:
/// JSON `null` becomes Lua `nil` (not mlua's null sentinel), so idioms like
/// `t.field or default` work on decoded data. Shared by `phi.json.decode`
/// and [`LuaExtension::with_global`] injection so both channels agree.
pub(super) fn json_to_lua(lua: &mlua::Lua, value: &serde_json::Value) -> mlua::Result<mlua::Value> {
    use mlua::LuaSerdeExt as _;
    lua.to_value_with(
        value,
        mlua::SerializeOptions::new()
            .serialize_none_to_null(false)
            .serialize_unit_to_null(false),
    )
}

pub(super) fn setup_phi_global(lua: &mlua::Lua) -> mlua::Result<()> {
    let globals = lua.globals();

    // Empty tables used by phi.*  methods
    let phi_handlers = lua.create_table()?;
    let phi_event_opts = lua.create_table()?;
    let phi_registered_events = lua.create_table()?;
    let phi_commands = lua.create_table()?;
    let phi_registered_commands = lua.create_table()?;
    let phi_tools = lua.create_table()?;
    let phi_registered_tools = lua.create_table()?;
    let phi_shortcuts = lua.create_table()?;
    let phi_registered_shortcuts = lua.create_table()?;
    let phi_flags = lua.create_table()?;
    let phi_registered_flags = lua.create_table()?;
    let phi_providers = lua.create_table()?;
    let phi_registered_providers = lua.create_table()?;
    let phi_renderers = lua.create_table()?;
    let phi_registered_renderers = lua.create_table()?;
    let phi_tool_renderers = lua.create_table()?;
    let phi_registered_tool_renderers = lua.create_table()?;
    let phi_themes = lua.create_table()?;
    let phi_registered_themes = lua.create_table()?;
    let phi_spinners = lua.create_table()?;
    let phi_registered_spinners = lua.create_table()?;
    let phi_thinking_expressions = lua.create_table()?;
    let phi_registered_thinking_expressions = lua.create_table()?;
    let phi_styles = lua.create_table()?;
    let phi_registered_styles = lua.create_table()?;
    let phi_overlays = lua.create_table()?;
    let phi_registered_overlays = lua.create_table()?;
    let phi_overlay_instances = lua.create_table()?;
    globals.set("_phi_handlers", phi_handlers)?;
    globals.set("_phi_event_opts", phi_event_opts)?;
    globals.set("_phi_registered_events", phi_registered_events)?;
    globals.set("_phi_commands", phi_commands)?;
    globals.set("_phi_registered_commands", phi_registered_commands)?;
    globals.set("_phi_tools", phi_tools)?;
    globals.set("_phi_registered_tools", phi_registered_tools)?;
    globals.set("_phi_shortcuts", phi_shortcuts)?;
    globals.set("_phi_registered_shortcuts", phi_registered_shortcuts)?;
    globals.set("_phi_flags", phi_flags)?;
    globals.set("_phi_registered_flags", phi_registered_flags)?;
    globals.set("_phi_providers", phi_providers)?;
    globals.set("_phi_registered_providers", phi_registered_providers)?;
    globals.set("_phi_renderers", phi_renderers)?;
    globals.set("_phi_registered_renderers", phi_registered_renderers)?;
    globals.set("_phi_tool_renderers", phi_tool_renderers)?;
    globals.set(
        "_phi_registered_tool_renderers",
        phi_registered_tool_renderers,
    )?;
    globals.set("_phi_themes", phi_themes)?;
    globals.set("_phi_registered_themes", phi_registered_themes)?;
    globals.set("_phi_spinners", phi_spinners)?;
    globals.set("_phi_registered_spinners", phi_registered_spinners)?;
    globals.set("_phi_thinking_expressions", phi_thinking_expressions)?;
    globals.set(
        "_phi_registered_thinking_expressions",
        phi_registered_thinking_expressions,
    )?;
    globals.set("_phi_styles", phi_styles)?;
    globals.set("_phi_registered_styles", phi_registered_styles)?;
    globals.set("_phi_overlays", phi_overlays)?;
    globals.set("_phi_registered_overlays", phi_registered_overlays)?;
    globals.set("_phi_overlay_instances", phi_overlay_instances)?;

    // Build the phi table
    let phi = lua.create_table()?;

    // phi.on(event_name, handler [, opts])
    // opts: { timeout = seconds } — raises this handler's dispatch watchdog
    // budget above the per-kind default. Needed by handlers that block on
    // phi.ui.ask (a timed-out interception handler contributes nothing, so
    // e.g. a gated tool would proceed).
    let on_fn = lua.create_function(
        |lua, (event_name, handler, opts): (String, mlua::Function, Option<mlua::Table>)| {
            let globals = lua.globals();
            let handlers: mlua::Table = globals.get("_phi_handlers")?;
            handlers.set(event_name.as_str(), handler)?;
            if let Some(opts) = opts {
                let opts_table: mlua::Table = globals.get("_phi_event_opts")?;
                opts_table.set(event_name.as_str(), opts)?;
            }
            let reg: mlua::Table = globals.get("_phi_registered_events")?;
            register_unique(&reg, event_name)?;
            Ok(())
        },
    )?;
    phi.set("on", on_fn)?;

    // phi.register_command(name, opts)
    // opts: { description = "...", args_hint = "...", context = "session"|"mux",
    //         aliases = { "..." }, completions = { "..." },
    //         handler = function(args) ... end }
    // The handler returns a string (shorthand for { message = ... }) or a
    // table { message = "...", actions = { { type = "Quit" },
    // { type = "OpenOverlay", data = { name = "phi:usage" } }, ... } } —
    // the same declarative UiAction vocabulary builtin commands use.
    let register_command_fn = lua.create_function(|lua, (name, opts): (String, mlua::Table)| {
        let globals = lua.globals();
        let commands: mlua::Table = globals.get("_phi_commands")?;
        commands.set(name.as_str(), opts)?;
        let reg: mlua::Table = globals.get("_phi_registered_commands")?;
        register_unique(&reg, name)?;
        Ok(())
    })?;
    phi.set("register_command", register_command_fn)?;

    // phi.register_tool(name, opts)
    // opts: { description = "...", params = {...}, handler = function(input) ... end,
    //         expose = "repl"|"native"|"both" (default "repl"),
    //         dispatch = "handler"|"python" (default "handler"),
    //         input_schema = { ... } (optional JSON Schema for the native
    //         tool definition; derived from params when absent) }
    // dispatch = "python" means the tool is implemented by a same-named
    // prelude function (no Lua handler needed); the runtime dispatches
    // native calls into the REPL env via `__phi_native_call__`.
    let register_tool_fn = lua.create_function(|lua, (name, opts): (String, mlua::Table)| {
        let globals = lua.globals();
        let tools: mlua::Table = globals.get("_phi_tools")?;
        tools.set(name.as_str(), opts)?;
        let reg: mlua::Table = globals.get("_phi_registered_tools")?;
        register_unique(&reg, name)?;
        Ok(())
    })?;
    phi.set("register_tool", register_tool_fn)?;

    // phi.register_shortcut(key, opts)
    // opts: { description = "...", handler = function() ... end }
    let register_shortcut_fn = lua.create_function(|lua, (key, opts): (String, mlua::Table)| {
        let globals = lua.globals();
        let shortcuts: mlua::Table = globals.get("_phi_shortcuts")?;
        shortcuts.set(key.as_str(), opts)?;
        let reg: mlua::Table = globals.get("_phi_registered_shortcuts")?;
        register_unique(&reg, key)?;
        Ok(())
    })?;
    phi.set("register_shortcut", register_shortcut_fn)?;

    // phi.register_flag(name, opts)
    // opts: { description = "...", type = "boolean"|"string", default = ... }
    let register_flag_fn = lua.create_function(|lua, (name, opts): (String, mlua::Table)| {
        let globals = lua.globals();
        let flags: mlua::Table = globals.get("_phi_flags")?;
        flags.set(name.as_str(), opts)?;
        let reg: mlua::Table = globals.get("_phi_registered_flags")?;
        register_unique(&reg, name)?;
        Ok(())
    })?;
    phi.set("register_flag", register_flag_fn)?;

    // phi.register_provider(name, config)
    // config: { base_url = "...", api_key = "...", api = "...", models = {...} }
    let register_provider_fn =
        lua.create_function(|lua, (name, config): (String, mlua::Table)| {
            let globals = lua.globals();
            let providers: mlua::Table = globals.get("_phi_providers")?;
            providers.set(name.as_str(), config)?;
            let reg: mlua::Table = globals.get("_phi_registered_providers")?;
            register_unique(&reg, name)?;
            Ok(())
        })?;
    phi.set("register_provider", register_provider_fn)?;

    // phi.register_message_renderer(custom_type, handler)
    // handler: function(message) -> string
    let register_renderer_fn =
        lua.create_function(|lua, (custom_type, handler): (String, mlua::Function)| {
            let globals = lua.globals();
            let renderers: mlua::Table = globals.get("_phi_renderers")?;

            let opts = lua.create_table()?;
            opts.set("handler", handler)?;
            renderers.set(custom_type.as_str(), opts)?;

            let reg: mlua::Table = globals.get("_phi_registered_renderers")?;
            register_unique(&reg, custom_type)?;
            Ok(())
        })?;
    phi.set("register_message_renderer", register_renderer_fn)?;

    // phi.register_tool_renderer(name, spec)
    // spec: { verb = "read", display_name = "Read File", category =
    //         "reads"|"edits"|"commands"|"searches"|"other",
    //         anchor = { { key = "path", style = "plain"|"path"|"pattern",
    //                      prefix = "in", skip_values = { "." } }, ... },
    //         body = { kind = "args"|"none"|"code", key = "cmd",
    //                  lang = "bash", prompt = "$" },
    //         result_lang = { kind = "none"|"fixed"|"path_arg",
    //                         lang = "...", key = "path" },
    //         exit_code_trailer = true }
    // Declarative display metadata for a tool (last write wins — restyling
    // a builtin tool is supported).
    let register_tool_renderer_fn =
        lua.create_function(|lua, (name, spec): (String, mlua::Table)| {
            let globals = lua.globals();
            let renderers: mlua::Table = globals.get("_phi_tool_renderers")?;
            renderers.set(name.as_str(), spec)?;
            let reg: mlua::Table = globals.get("_phi_registered_tool_renderers")?;
            register_unique(&reg, name)?;
            Ok(())
        })?;
    phi.set("register_tool_renderer", register_tool_renderer_fn)?;

    // phi.register_theme(name, palette)
    // palette: { foreground = {r,g,b}, background = {r,g,b}, accent = {r,g,b}, ... }
    let register_theme_fn =
        lua.create_function(|lua, (name, palette): (String, mlua::Table)| {
            let globals = lua.globals();
            let themes: mlua::Table = globals.get("_phi_themes")?;
            themes.set(name.as_str(), palette)?;
            let reg: mlua::Table = globals.get("_phi_registered_themes")?;
            register_unique(&reg, name)?;
            Ok(())
        })?;
    phi.set("register_theme", register_theme_fn)?;

    // phi.register_spinner(name, opts)
    // opts: { frames = {"a", "b", ...}, frame_interval_ms = 80 }
    let register_spinner_fn = lua.create_function(|lua, (name, opts): (String, mlua::Table)| {
        let globals = lua.globals();
        let spinners: mlua::Table = globals.get("_phi_spinners")?;
        spinners.set(name.as_str(), opts)?;
        let reg: mlua::Table = globals.get("_phi_registered_spinners")?;
        register_unique(&reg, name)?;
        Ok(())
    })?;
    phi.set("register_spinner", register_spinner_fn)?;

    // phi.register_thinking_expression(name, opts)
    // opts: { expressions = {"thinking...", ...}, tool_expression = "working...",
    //         expression_interval_ms = 2000 }
    let register_thinking_expr_fn =
        lua.create_function(|lua, (name, opts): (String, mlua::Table)| {
            let globals = lua.globals();
            let exprs: mlua::Table = globals.get("_phi_thinking_expressions")?;
            exprs.set(name.as_str(), opts)?;
            let reg: mlua::Table = globals.get("_phi_registered_thinking_expressions")?;
            register_unique(&reg, name)?;
            Ok(())
        })?;
    phi.set("register_thinking_expression", register_thinking_expr_fn)?;

    // phi.register_style(name, opts) — a style pack (WS10): a bundle of the
    // existing visual shapes activated as a unit by `/style <name>`.
    // opts: { theme = { foreground = {r,g,b}, ... },          -- palette table
    //         spinner = { frames = {...}, frame_interval_ms = 80 },
    //         thinking = { expressions = {...}, tool_expression = "...",
    //                      expression_interval_ms = 2000 },
    //         tool_renderers = { bash = { verb = "exec", ... }, ... } }
    // Every member is optional; absent members fall back to the base
    // registrations while the style is active.
    let register_style_fn = lua.create_function(|lua, (name, opts): (String, mlua::Table)| {
        let globals = lua.globals();
        let styles: mlua::Table = globals.get("_phi_styles")?;
        styles.set(name.as_str(), opts)?;
        let reg: mlua::Table = globals.get("_phi_registered_styles")?;
        register_unique(&reg, name)?;
        Ok(())
    })?;
    phi.set("register_style", register_style_fn)?;

    // phi.register_overlay(name, opts)
    // opts: {
    //   description = "...",
    //   toggle_key = "ctrl+?",                        -- optional
    //   init = function(payload) return state end,    -- optional; payload is
    //         -- the host's open payload as a table (nil for toggle opens):
    //         -- generic JSON from UiAction::OpenOverlay, or the typed
    //         -- payloads { kind = "model", root, current, models } /
    //         -- { kind = "help", entries }.
    //   render = function(state, ctx) ... end,        -- ctx = { cols, rows,
    //         -- palette = { fg={r,g,b}, bg, muted, border, accent, accent_2,
    //         -- surface_raised, statusbar_bg } }. Returns either an array of
    //         -- plain text lines (drawn in a bordered box) or draw ops:
    //         -- { { op="box"|"fill"|"text"|"clear"|"scrollbar", ... } } with
    //         -- colours as {r,g,b} arrays or palette role names ("accent").
    //   handle_key = function(state, key, cols, rows) -- optional; key =
    //         -- { code = "esc"|"enter"|"up"|...|"char", char = "q",
    //         --   ctrl, shift, alt }. Return nil/"none", "close",
    //         -- { submit = "..." }, { respond = "..." } (fires the typed
    //         -- payload's response channel), or { message = "...",
    //         -- actions = {...} } (same vocabulary as command handlers).
    //         -- Without handle_key, Esc/Enter/q close the overlay.
    //   handle_scroll = function(state, delta, cols, rows) end, -- optional
    // }
    let register_overlay_fn = lua.create_function(|lua, (name, opts): (String, mlua::Table)| {
        let globals = lua.globals();
        let overlays: mlua::Table = globals.get("_phi_overlays")?;
        overlays.set(name.as_str(), opts)?;
        let reg: mlua::Table = globals.get("_phi_registered_overlays")?;
        register_unique(&reg, name)?;
        Ok(())
    })?;
    phi.set("register_overlay", register_overlay_fn)?;

    // phi.fs — filesystem helpers (portable, no io.popen hacks).
    // Extensions that need file discovery (e.g. skills) use these instead
    // of shell-out tricks. Wraps std::fs in a thin Lua-callable layer.
    let fs_table = lua.create_table()?;

    let fs_readfile =
        lua.create_function(|_, path: String| Ok(std::fs::read_to_string(&path).ok()))?;
    fs_table.set("readfile", fs_readfile)?;

    let fs_writefile = lua.create_function(|_, (path, content): (String, String)| {
        Ok(std::fs::write(&path, &content).is_ok())
    })?;
    fs_table.set("writefile", fs_writefile)?;

    let fs_exists =
        lua.create_function(|_, path: String| Ok(std::path::Path::new(&path).exists()))?;
    fs_table.set("exists", fs_exists)?;

    let fs_isdir =
        lua.create_function(|_, path: String| Ok(std::path::Path::new(&path).is_dir()))?;
    fs_table.set("isdir", fs_isdir)?;

    let fs_readdir = lua.create_function(|_, path: String| {
        let mut entries: Vec<String> = match std::fs::read_dir(&path) {
            Ok(rd) => rd
                .flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect(),
            Err(_) => return Ok(None), // nil for missing dirs
        };
        entries.sort();
        Ok(Some(entries))
    })?;
    fs_table.set("readdir", fs_readdir)?;

    let fs_mkdir_p =
        lua.create_function(|_, path: String| Ok(std::fs::create_dir_all(&path).is_ok()))?;
    fs_table.set("mkdir_p", fs_mkdir_p)?;

    let fs_canonicalize = lua.create_function(|_, path: String| {
        Ok(std::fs::canonicalize(&path)
            .ok()
            .map(|p| p.to_string_lossy().into_owned()))
    })?;
    fs_table.set("canonicalize", fs_canonicalize)?;

    phi.set("fs", fs_table)?;

    // phi.json — JSON encode/decode backed by serde_json, the same parser
    // the host uses everywhere. Extensions that persist or exchange JSON
    // (e.g. approvals) use these instead of hand-rolling a parser.
    //
    // - `phi.json.encode(value)` → string. Raises on values JSON can't
    //   carry (functions, userdata). An empty table encodes as `{}`.
    // - `phi.json.decode(s)` → value, or `nil, err` on malformed input.
    //   JSON `null` decodes to Lua `nil` (so `t.field or default` works);
    //   don't rely on nulls inside arrays.
    let json_table = lua.create_table()?;

    let json_encode = lua.create_function(|lua, value: mlua::Value| {
        use mlua::LuaSerdeExt as _;
        let json: serde_json::Value = lua
            .from_value(value)
            .map_err(|e| mlua::Error::runtime(format!("phi.json.encode: {e}")))?;
        serde_json::to_string(&json)
            .map_err(|e| mlua::Error::runtime(format!("phi.json.encode: {e}")))
    })?;
    json_table.set("encode", json_encode)?;

    let json_decode = lua.create_function(|lua, s: String| {
        match serde_json::from_str::<serde_json::Value>(&s) {
            Ok(json) => Ok((Some(json_to_lua(lua, &json)?), None)),
            Err(e) => Ok((None, Some(e.to_string()))),
        }
    })?;
    json_table.set("decode", json_decode)?;

    phi.set("json", json_table)?;
    globals.set("phi", phi)?;
    Ok(())
}

/// Attach `phi.ui` — the ask-the-user namespace (pi's `ctx.ui` analog),
/// backed by the runtime's late-bound [`phi_ext::UiHandle`]. Separate from
/// [`setup_phi_global`] because it needs the registration host.
///
/// - `phi.ui.has_ui()` → whether an interactive host is connected.
/// - `phi.ui.ask{ title, body, options = { "Allow", { label = "Deny",
///   detail = "..." } }, wait = seconds (default 300) }` → blocks the Lua
///   thread (event/command handling for this extension queues behind it) and
///   returns `index, outcome`: a 1-based option index with outcome
///   `"answer"`, or `nil` with `"dismissed"` / `"no_ui"` / `"timeout"`.
///   Pair a blocking ask with a `phi.on` `timeout` above `wait`, otherwise
///   the dispatch watchdog stops waiting first.
pub(super) fn attach_ui_namespace(lua: &mlua::Lua, ui: phi_ext::UiHandle) -> mlua::Result<()> {
    use std::time::Duration;

    let phi: mlua::Table = lua.globals().get("phi")?;
    let ui_table = lua.create_table()?;

    let ui_has = ui.clone();
    ui_table.set(
        "has_ui",
        lua.create_function(move |_, ()| Ok(ui_has.has_ui()))?,
    )?;

    ui_table.set(
        "ask",
        lua.create_function(move |_, opts: mlua::Table| {
            let title: String = opts.get("title").unwrap_or_default();
            let body: String = opts.get("body").unwrap_or_default();
            let mut options = Vec::new();
            if let Ok(Some(t)) = opts.get::<Option<mlua::Table>>("options") {
                for i in 1..=t.raw_len() {
                    match t.get::<mlua::Value>(i)? {
                        mlua::Value::String(s) => {
                            options.push(phi_ext::UiPromptOption::new(s.to_str()?.to_owned()));
                        }
                        mlua::Value::Table(o) => {
                            let label: String = o.get("label").unwrap_or_default();
                            let mut option = phi_ext::UiPromptOption::new(label);
                            if let Ok(Some(detail)) = o.get::<Option<String>>("detail") {
                                option = option.with_detail(detail);
                            }
                            options.push(option);
                        }
                        _ => {}
                    }
                }
            }
            if options.is_empty() {
                return Err(mlua::Error::runtime(
                    "phi.ui.ask requires a non-empty `options` array",
                ));
            }
            let wait = opts
                .get::<Option<f64>>("wait")
                .ok()
                .flatten()
                .unwrap_or(300.0);
            let outcome = ui.ask(
                phi_ext::UiPrompt {
                    title,
                    body,
                    options,
                },
                Duration::from_secs_f64(wait.max(0.0)),
            );
            Ok(match outcome {
                phi_ext::UiAskOutcome::Answer(i) => (Some(i + 1), "answer"),
                phi_ext::UiAskOutcome::Dismissed => (None, "dismissed"),
                phi_ext::UiAskOutcome::NoUi => (None, "no_ui"),
                phi_ext::UiAskOutcome::TimedOut => (None, "timeout"),
            })
        })?,
    )?;

    phi.set("ui", ui_table)?;
    Ok(())
}

/// Append `value` to a registration table unless it's already present.
/// Re-registration (e.g. a hot reload) must not duplicate the entry — the
/// host dispatches once per registered name.
fn register_unique<V: mlua::IntoLua + Clone + PartialEq + mlua::FromLua>(reg: &mlua::Table, value: V) -> mlua::Result<()> {
    let len = reg.raw_len();
    let already = (1..=len).any(|i| {
        reg.raw_get::<V>(i).is_ok_and(|existing| existing == value)
    });
    if !already {
        reg.raw_set(len + 1, value)?;
    }
    Ok(())
}
