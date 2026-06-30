pub(super) fn setup_phi_global(lua: &mlua::Lua) -> mlua::Result<()> {
    let globals = lua.globals();

    // Empty tables used by phi.*  methods
    let phi_handlers = lua.create_table()?;
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
    let phi_themes = lua.create_table()?;
    let phi_registered_themes = lua.create_table()?;
    let phi_spinners = lua.create_table()?;
    let phi_registered_spinners = lua.create_table()?;
    let phi_thinking_expressions = lua.create_table()?;
    let phi_registered_thinking_expressions = lua.create_table()?;
    let phi_overlays = lua.create_table()?;
    let phi_registered_overlays = lua.create_table()?;
    globals.set("_phi_handlers", phi_handlers)?;
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
    globals.set("_phi_themes", phi_themes)?;
    globals.set("_phi_registered_themes", phi_registered_themes)?;
    globals.set("_phi_spinners", phi_spinners)?;
    globals.set("_phi_registered_spinners", phi_registered_spinners)?;
    globals.set("_phi_thinking_expressions", phi_thinking_expressions)?;
    globals.set(
        "_phi_registered_thinking_expressions",
        phi_registered_thinking_expressions,
    )?;
    globals.set("_phi_overlays", phi_overlays)?;
    globals.set("_phi_registered_overlays", phi_registered_overlays)?;

    // Build the phi table
    let phi = lua.create_table()?;

    // phi.on(event_name, handler)
    let on_fn = lua.create_function(|lua, (event_name, handler): (String, mlua::Function)| {
        let globals = lua.globals();
        let handlers: mlua::Table = globals.get("_phi_handlers")?;
        handlers.set(event_name.as_str(), handler)?;
        let reg: mlua::Table = globals.get("_phi_registered_events")?;
        let len = reg.raw_len();
        reg.raw_set(len + 1, event_name)?;
        Ok(())
    })?;
    phi.set("on", on_fn)?;

    // phi.register_command(name, opts)
    let register_command_fn = lua.create_function(|lua, (name, opts): (String, mlua::Table)| {
        let globals = lua.globals();
        let commands: mlua::Table = globals.get("_phi_commands")?;
        commands.set(name.as_str(), opts)?;
        let reg: mlua::Table = globals.get("_phi_registered_commands")?;
        let len = reg.raw_len();
        reg.raw_set(len + 1, name)?;
        Ok(())
    })?;
    phi.set("register_command", register_command_fn)?;

    // phi.register_tool(name, opts)
    // opts: { description = "...", params = {...}, handler = function(input) ... end }
    let register_tool_fn = lua.create_function(|lua, (name, opts): (String, mlua::Table)| {
        let globals = lua.globals();
        let tools: mlua::Table = globals.get("_phi_tools")?;
        tools.set(name.as_str(), opts)?;
        let reg: mlua::Table = globals.get("_phi_registered_tools")?;
        let len = reg.raw_len();
        reg.raw_set(len + 1, name)?;
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
        let len = reg.raw_len();
        reg.raw_set(len + 1, key)?;
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
        let len = reg.raw_len();
        reg.raw_set(len + 1, name)?;
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
            let len = reg.raw_len();
            reg.raw_set(len + 1, name)?;
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
            let len = reg.raw_len();
            reg.raw_set(len + 1, custom_type)?;
            Ok(())
        })?;
    phi.set("register_message_renderer", register_renderer_fn)?;

    // phi.register_theme(name, palette)
    // palette: { foreground = {r,g,b}, background = {r,g,b}, accent = {r,g,b}, ... }
    let register_theme_fn =
        lua.create_function(|lua, (name, palette): (String, mlua::Table)| {
            let globals = lua.globals();
            let themes: mlua::Table = globals.get("_phi_themes")?;
            themes.set(name.as_str(), palette)?;
            let reg: mlua::Table = globals.get("_phi_registered_themes")?;
            let len = reg.raw_len();
            reg.raw_set(len + 1, name)?;
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
        let len = reg.raw_len();
        reg.raw_set(len + 1, name)?;
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
            let len = reg.raw_len();
            reg.raw_set(len + 1, name)?;
            Ok(())
        })?;
    phi.set("register_thinking_expression", register_thinking_expr_fn)?;

    // phi.register_overlay(name, opts)
    // opts: { description = "...", toggle_key = "ctrl+?", render = function() ... end }
    // The render function returns a table of text lines (array of strings).
    // The TUI draws these lines in a bordered box.
    let register_overlay_fn = lua.create_function(|lua, (name, opts): (String, mlua::Table)| {
        let globals = lua.globals();
        let overlays: mlua::Table = globals.get("_phi_overlays")?;
        overlays.set(name.as_str(), opts)?;
        let reg: mlua::Table = globals.get("_phi_registered_overlays")?;
        let len = reg.raw_len();
        reg.raw_set(len + 1, name)?;
        Ok(())
    })?;
    phi.set("register_overlay", register_overlay_fn)?;

    globals.set("phi", phi)?;
    Ok(())
}
