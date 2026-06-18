use anyhow::Result;

use super::convert::{json_to_lua, lua_value_to_json_string};

pub(super) fn call_lua_event(lua: &mlua::Lua, event_name: &str, payload_json: &str) -> Result<Option<String>> {
    let globals = lua.globals();
    let handlers: mlua::Table = globals.get("_phi_handlers")?;
    let handler: Option<mlua::Function> = handlers.get(event_name)?;
    let Some(handler) = handler else {
        return Ok(None);
    };
    let payload_val: serde_json::Value = serde_json::from_str(payload_json)?;
    let lua_payload = json_to_lua(lua, &payload_val)?;
    let result: mlua::Value = handler.call(lua_payload)?;
    lua_value_to_json_string(result)
}

pub(super) fn call_lua_command(lua: &mlua::Lua, name: &str, args: &str) -> Result<Option<String>> {
    let commands: mlua::Table = lua.globals().get("_phi_commands")?;
    let cmd: Option<mlua::Table> = commands.get(name)?;
    let Some(cmd) = cmd else {
        return Ok(None);
    };
    let handler: mlua::Function = cmd.get("handler")?;
    let result: mlua::Value = handler.call(args)?;
    // Extract message string
    match result {
        mlua::Value::Table(t) => {
            let msg: Option<String> = t.get("message").ok();
            Ok(msg)
        }
        mlua::Value::String(s) => Ok(Some(s.to_str()?.to_owned())),
        _ => Ok(None),
    }
}
