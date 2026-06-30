use anyhow::Result;

use super::convert::{json_to_lua, lua_value_to_json_string};

pub(super) fn call_lua_event(
    lua: &mlua::Lua,
    event_name: &str,
    payload_json: &str,
) -> Result<Option<String>> {
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

pub(super) fn call_lua_tool(lua: &mlua::Lua, name: &str, input: &str) -> Result<Option<String>> {
    let tools: mlua::Table = lua.globals().get("_phi_tools")?;
    let tool: Option<mlua::Table> = tools.get(name)?;
    let Some(tool) = tool else {
        return Ok(None);
    };
    let handler: mlua::Function = tool.get("handler")?;
    let result: mlua::Value = handler.call(input)?;
    // Extract content string from the result
    match result {
        mlua::Value::Table(t) => {
            let content: Option<String> = t.get("content").ok();
            Ok(content)
        }
        mlua::Value::String(s) => Ok(Some(s.to_str()?.to_owned())),
        _ => Ok(None),
    }
}

/// Call a Lua-registered overlay's render function.
///
/// The render function returns a table of text lines (array of strings).
/// We serialize this to a JSON array string for the Rust side to draw.
pub(super) fn call_lua_overlay_render(lua: &mlua::Lua, name: &str) -> Result<Option<String>> {
    let overlays: mlua::Table = lua.globals().get("_phi_overlays")?;
    let overlay: Option<mlua::Table> = overlays.get(name)?;
    let Some(overlay) = overlay else {
        return Ok(None);
    };
    let render_fn: Option<mlua::Function> = overlay.get("render").ok();
    let Some(render_fn) = render_fn else {
        return Ok(None);
    };
    let result: mlua::Value = render_fn.call(())?;
    // Convert the result (table of strings) to a JSON array string
    match result {
        mlua::Value::Table(t) => {
            let mut lines = Vec::new();
            for pair in t.pairs::<mlua::Value, mlua::String>() {
                let (_, s) = pair?;
                lines.push(s.to_str()?.to_owned());
            }
            let json = serde_json::to_string(&lines)?;
            Ok(Some(json))
        }
        mlua::Value::String(s) => {
            // Single string: wrap in an array
            let json = serde_json::to_string(&[s.to_str()?.to_owned()])?;
            Ok(Some(json))
        }
        _ => Ok(Some("[]".to_owned())),
    }
}
