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

/// Invoke a Lua command handler. Returns the full result as JSON so the
/// caller can decode `message` *and* `actions` (a bare string return is
/// shorthand for `{ message = ... }`).
pub(super) fn call_lua_command(lua: &mlua::Lua, name: &str, args: &str) -> Result<Option<String>> {
    let commands: mlua::Table = lua.globals().get("_phi_commands")?;
    let cmd: Option<mlua::Table> = commands.get(name)?;
    let Some(cmd) = cmd else {
        return Ok(None);
    };
    let handler: mlua::Function = cmd.get("handler")?;
    let result: mlua::Value = handler.call(args)?;
    match result {
        mlua::Value::Table(t) => lua_value_to_json_string(mlua::Value::Table(t)),
        mlua::Value::String(s) => Ok(Some(serde_json::to_string(
            &serde_json::json!({ "message": s.to_str()?.to_owned() }),
        )?)),
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

/// Look up an overlay's registration table.
fn lua_overlay(lua: &mlua::Lua, name: &str) -> Result<Option<mlua::Table>> {
    let overlays: mlua::Table = lua.globals().get("_phi_overlays")?;
    Ok(overlays.get(name)?)
}

/// Per-open overlay states, keyed by host-generated instance id. Lives in a
/// VM global so state survives between the host's render/key/scroll calls;
/// freed by [`drop_lua_overlay_instance`] when the host drops the overlay.
fn overlay_instances(lua: &mlua::Lua) -> Result<mlua::Table> {
    Ok(lua.globals().get("_phi_overlay_instances")?)
}

/// Open an overlay instance: call its `init(payload)` (when defined) and
/// store the returned state under `instance`. Overlays without `init` get an
/// empty table so `render`/`handle_key` always receive a state value.
pub(super) fn call_lua_overlay_init(
    lua: &mlua::Lua,
    name: &str,
    instance: u64,
    payload_json: Option<&str>,
) -> Result<Option<String>> {
    let Some(overlay) = lua_overlay(lua, name)? else {
        return Ok(None);
    };
    let init_fn: Option<mlua::Function> = overlay.get("init").ok().flatten();
    let state: mlua::Value = match init_fn {
        Some(init) => {
            let payload = match payload_json {
                Some(json) => {
                    let val: serde_json::Value = serde_json::from_str(json)?;
                    json_to_lua(lua, &val)?
                }
                None => mlua::Value::Nil,
            };
            init.call(payload)?
        }
        None => mlua::Value::Table(lua.create_table()?),
    };
    overlay_instances(lua)?.set(instance, state)?;
    Ok(None)
}

/// Call a Lua-registered overlay's render function with the instance state
/// and a ctx table (`{ cols, rows, palette }`).
///
/// The render function may return a table of text lines (legacy text-box
/// mode) or draw ops (an array of `{ op = "text"|"box"|"fill"|..., ... }`
/// tables, or `{ ops = {...} }`); either serializes to JSON for the Rust
/// side to draw.
pub(super) fn call_lua_overlay_render(
    lua: &mlua::Lua,
    name: &str,
    instance: u64,
    ctx_json: &str,
) -> Result<Option<String>> {
    let Some(overlay) = lua_overlay(lua, name)? else {
        return Ok(None);
    };
    let render_fn: Option<mlua::Function> = overlay.get("render").ok().flatten();
    let Some(render_fn) = render_fn else {
        return Ok(None);
    };
    let state: mlua::Value = overlay_instances(lua)?.get(instance)?;
    let ctx_val: serde_json::Value = serde_json::from_str(ctx_json)?;
    let ctx = json_to_lua(lua, &ctx_val)?;
    let result: mlua::Value = render_fn.call((state, ctx))?;
    match result {
        mlua::Value::Table(t) => lua_value_to_json_string(mlua::Value::Table(t)),
        mlua::Value::String(s) => {
            // Single string: wrap in an array (legacy line mode)
            let json = serde_json::to_string(&[s.to_str()?.to_owned()])?;
            Ok(Some(json))
        }
        _ => Ok(Some("[]".to_owned())),
    }
}

/// Forward a key event to a Lua overlay's `handle_key(state, key, cols, rows)`.
/// The outcome is returned as JSON: a string (`"close"`, `"none"`) or a
/// table (`{ submit = ... }`, `{ respond = ... }`, `{ message/actions }`).
pub(super) fn call_lua_overlay_key(
    lua: &mlua::Lua,
    name: &str,
    instance: u64,
    key_json: &str,
    cols: u16,
    rows: u16,
) -> Result<Option<String>> {
    let Some(overlay) = lua_overlay(lua, name)? else {
        return Ok(None);
    };
    let key_fn: Option<mlua::Function> = overlay.get("handle_key").ok().flatten();
    let Some(key_fn) = key_fn else {
        return Ok(None);
    };
    let state: mlua::Value = overlay_instances(lua)?.get(instance)?;
    let key_val: serde_json::Value = serde_json::from_str(key_json)?;
    let key = json_to_lua(lua, &key_val)?;
    let result: mlua::Value = key_fn.call((state, key, cols, rows))?;
    match result {
        mlua::Value::String(s) => Ok(Some(serde_json::to_string(&s.to_str()?.to_owned())?)),
        other => lua_value_to_json_string(other),
    }
}

/// Forward a mouse-wheel scroll to a Lua overlay's
/// `handle_scroll(state, delta, cols, rows)`.
pub(super) fn call_lua_overlay_scroll(
    lua: &mlua::Lua,
    name: &str,
    instance: u64,
    delta: i32,
    cols: u16,
    rows: u16,
) -> Result<Option<String>> {
    let Some(overlay) = lua_overlay(lua, name)? else {
        return Ok(None);
    };
    let scroll_fn: Option<mlua::Function> = overlay.get("handle_scroll").ok().flatten();
    let Some(scroll_fn) = scroll_fn else {
        return Ok(None);
    };
    let state: mlua::Value = overlay_instances(lua)?.get(instance)?;
    scroll_fn.call::<()>((state, delta, cols, rows))?;
    Ok(None)
}

/// Free an overlay instance's Lua-side state (host dropped the overlay).
pub(super) fn drop_lua_overlay_instance(lua: &mlua::Lua, instance: u64) -> Result<()> {
    overlay_instances(lua)?.set(instance, mlua::Value::Nil)?;
    Ok(())
}
