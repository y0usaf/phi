//! The `phi.*` globals — the canonical extension API. Registration state
//! lives Lua-side in a registry-held table (handlers stay Lua functions);
//! name/description metadata is mirrored into [`Meta`] so the host can
//! answer queries without a VM round-trip.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::{CommandInfo, ToolInfo};

/// Key of the registration table in the Lua named registry (not a global,
/// so extension code can't clobber it by accident).
pub(crate) const REGISTRY_KEY: &str = "phi-host";

/// Host-side mirror of registration metadata.
#[derive(Debug, Default)]
pub(crate) struct Meta {
    pub commands: Vec<CommandInfo>,
    /// Tools in registration order — Lua-registered and native entries
    /// share this one list (doctrine 01: one registry, two handler homes).
    pub tools: Vec<ToolInfo>,
    /// Event name → number of subscribed handlers.
    pub events: BTreeMap<String, usize>,
}

/// The registration table: `{ commands = {}, tools = {}, events = {}, source = "" }`.
pub(crate) fn registry_table(lua: &mlua::Lua) -> mlua::Result<mlua::Table> {
    lua.named_registry_value::<mlua::Table>(REGISTRY_KEY)
}

/// Source key of the extension currently being loaded (set by the load
/// path so registrations are attributable).
pub(crate) fn current_source(lua: &mlua::Lua) -> String {
    registry_table(lua)
        .and_then(|t| t.get::<String>("source"))
        .unwrap_or_else(|_| "<unknown>".to_owned())
}

/// Create the registration table and install the `phi` global.
///
/// Surface (grown only alongside an exercising example — DESIGN.md):
/// - `phi.register_command{ name, handler, description? }`
/// - `phi.register_tool{ name, description, parameters?, handler }`
/// - `phi.on(event, fn)` — open string vocabulary, no closed enum.
/// - `phi.commands()` — snapshot of registered commands
///   (`{ name, description?, source }` each), registration order.
pub(crate) fn install(lua: &mlua::Lua, meta: Arc<Mutex<Meta>>) -> mlua::Result<()> {
    let registry = lua.create_table()?;
    registry.set("commands", lua.create_table()?)?;
    registry.set("tools", lua.create_table()?)?;
    registry.set("events", lua.create_table()?)?;
    registry.set("source", "<host>")?;
    lua.set_named_registry_value(REGISTRY_KEY, registry)?;

    let phi = lua.create_table()?;

    let register_command = {
        let meta = Arc::clone(&meta);
        lua.create_function(move |lua, spec: mlua::Table| {
            let name: String = spec
                .get::<Option<String>>("name")?
                .filter(|n| !n.trim().is_empty())
                .ok_or_else(|| {
                    mlua::Error::runtime("register_command: 'name' must be a non-empty string")
                })?;
            let handler: mlua::Function = spec
                .get::<Option<mlua::Function>>("handler")?
                .ok_or_else(|| {
                    mlua::Error::runtime(format!(
                        "register_command('{name}'): 'handler' must be a function"
                    ))
                })?;
            let description: Option<String> = spec.get("description")?;
            let source = current_source(lua);

            let commands: mlua::Table = registry_table(lua)?.get("commands")?;
            if let Some(existing) = commands.get::<Option<mlua::Table>>(name.as_str())? {
                let prior: String = existing
                    .get("source")
                    .unwrap_or_else(|_| "<unknown>".to_owned());
                return Err(mlua::Error::runtime(format!(
                    "register_command('{name}'): already registered by {prior}"
                )));
            }
            let entry = lua.create_table()?;
            entry.set("handler", handler)?;
            entry.set("description", description.clone())?;
            entry.set("source", source.clone())?;
            commands.set(name.as_str(), entry)?;

            let mut meta = meta.lock().expect("meta lock");
            meta.commands.push(CommandInfo {
                name,
                description,
                source,
            });
            Ok(())
        })?
    };
    phi.set("register_command", register_command)?;

    let register_tool = {
        let meta = Arc::clone(&meta);
        lua.create_function(move |lua, spec: mlua::Table| {
            let name: String = spec
                .get::<Option<String>>("name")?
                .filter(|n| !n.trim().is_empty())
                .ok_or_else(|| {
                    mlua::Error::runtime("register_tool: 'name' must be a non-empty string")
                })?;
            let description: String = spec
                .get::<Option<String>>("description")?
                .filter(|d| !d.trim().is_empty())
                .ok_or_else(|| {
                    mlua::Error::runtime(format!(
                        "register_tool('{name}'): 'description' must be a non-empty string \
                         (the model reads it)"
                    ))
                })?;
            let handler: mlua::Function = spec
                .get::<Option<mlua::Function>>("handler")?
                .ok_or_else(|| {
                    mlua::Error::runtime(format!(
                        "register_tool('{name}'): 'handler' must be a function"
                    ))
                })?;
            // JSON Schema for the arguments, as a plain table. Defaults to
            // an object schema with no declared properties.
            let input_schema = match spec.get::<Option<mlua::Table>>("parameters")? {
                Some(t) => crate::convert::lua_to_json(mlua::Value::Table(t))?,
                None => serde_json::json!({ "type": "object", "properties": {} }),
            };
            let source = current_source(lua);

            let mut meta = meta.lock().expect("meta lock");
            if let Some(prior) = meta.tools.iter().find(|t| t.name == name) {
                return Err(mlua::Error::runtime(format!(
                    "register_tool('{name}'): already registered by {}",
                    prior.source
                )));
            }

            let tools: mlua::Table = registry_table(lua)?.get("tools")?;
            let entry = lua.create_table()?;
            entry.set("handler", handler)?;
            entry.set("source", source.clone())?;
            tools.set(name.as_str(), entry)?;

            meta.tools.push(ToolInfo {
                name,
                description,
                input_schema,
                source,
            });
            Ok(())
        })?
    };
    phi.set("register_tool", register_tool)?;

    let on = {
        let meta = Arc::clone(&meta);
        lua.create_function(move |lua, (event, handler): (String, mlua::Function)| {
            if event.trim().is_empty() {
                return Err(mlua::Error::runtime(
                    "on: event name must be a non-empty string",
                ));
            }
            let events: mlua::Table = registry_table(lua)?.get("events")?;
            let list: mlua::Table = match events.get::<Option<mlua::Table>>(event.as_str())? {
                Some(list) => list,
                None => {
                    let list = lua.create_table()?;
                    events.set(event.as_str(), &list)?;
                    list
                }
            };
            let entry = lua.create_table()?;
            entry.set("fn", handler)?;
            entry.set("source", current_source(lua))?;
            list.push(entry)?;

            let mut meta = meta.lock().expect("meta lock");
            *meta.events.entry(event).or_insert(0) += 1;
            Ok(())
        })?
    };
    phi.set("on", on)?;

    let commands = {
        let meta = Arc::clone(&meta);
        lua.create_function(move |lua, ()| {
            let meta = meta.lock().expect("meta lock");
            let list = lua.create_table()?;
            for (i, cmd) in meta.commands.iter().enumerate() {
                let entry = lua.create_table()?;
                entry.set("name", cmd.name.as_str())?;
                entry.set("description", cmd.description.as_deref())?;
                entry.set("source", cmd.source.as_str())?;
                list.set(i + 1, entry)?;
            }
            Ok(list)
        })?
    };
    phi.set("commands", commands)?;

    lua.globals().set("phi", phi)?;
    Ok(())
}
