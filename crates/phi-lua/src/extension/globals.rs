pub(super) fn setup_phi_global(lua: &mlua::Lua) -> mlua::Result<()> {
    let globals = lua.globals();

    // Empty tables used by phi.*  methods
    let phi_handlers = lua.create_table()?;
    let phi_registered_events = lua.create_table()?;
    let phi_commands = lua.create_table()?;
    let phi_registered_commands = lua.create_table()?;
    globals.set("_phi_handlers", phi_handlers)?;
    globals.set("_phi_registered_events", phi_registered_events)?;
    globals.set("_phi_commands", phi_commands)?;
    globals.set("_phi_registered_commands", phi_registered_commands)?;

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

    globals.set("phi", phi)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Extension impl
// ---------------------------------------------------------------------------
