//! The `pi` API table — the canonical extension surface, named after the
//! spec's `ExtensionAPI` parameter so example translations stay literal
//! (`export default function (pi) { ... }` → `local pi = ...`).
//!
//! Registration state lives Lua-side in a registry-held table (handlers
//! stay Lua functions); the host reads it at dispatch time. The table is
//! kept in the named registry, not a global, so extension code can't
//! clobber it by accident.
//!
//! Surface at WS1.2 (grown only alongside an exercising example):
//! - `pi.on(event, fn)` — open string vocabulary, no closed enum.
//! - `pi.sleep(ms)` — first awaitable host future: suspends the handler
//!   coroutine without burning watchdog budget (superset of pi's
//!   `await new Promise(setTimeout)`; divergence 3).
//! - `pi.register_tool(def)` — spec `registerTool` (`loader.ts`):
//!   per-extension map, re-registration replaces in place.
//! - `pi.register_command(name, opts)` — spec `registerCommand`
//!   (`loader.ts`): `{ name, ...opts }` into the per-extension map.

use std::collections::{HashMap, HashSet};

/// Key of the registration table in the Lua named registry.
pub(crate) const REGISTRY_KEY: &str = "phi-host";

/// The registration table:
/// `{ events = {}, exts = {}, ext_order = {}, source = "<host>" }`.
pub(crate) fn registry_table(lua: &mlua::Lua) -> mlua::Result<mlua::Table> {
    lua.named_registry_value::<mlua::Table>(REGISTRY_KEY)
}

/// Source key of the extension currently being loaded or dispatched (set
/// by the load/dispatch paths so registrations are attributable).
pub(crate) fn current_source(lua: &mlua::Lua) -> String {
    registry_table(lua)
        .and_then(|t| t.get::<String>("source"))
        .unwrap_or_else(|_| "<unknown>".to_owned())
}

pub(crate) fn set_current_source(lua: &mlua::Lua, source: &str) {
    if let Ok(registry) = registry_table(lua) {
        let _ = registry.set("source", source);
    }
}

/// Get-or-create the per-extension registration entry (spec: the
/// `Extension` object in `types.ts` — per-extension maps of tools and
/// commands). `*_order` arrays make JS `Map` insertion order explicit,
/// which Lua hash parts don't preserve.
fn ext_entry(lua: &mlua::Lua, source: &str) -> mlua::Result<mlua::Table> {
    let registry = registry_table(lua)?;
    let exts: mlua::Table = registry.get("exts")?;
    if let Some(entry) = exts.get::<Option<mlua::Table>>(source)? {
        return Ok(entry);
    }
    let entry = lua.create_table()?;
    entry.set("tools", lua.create_table()?)?;
    entry.set("tool_order", lua.create_table()?)?;
    entry.set("commands", lua.create_table()?)?;
    entry.set("command_order", lua.create_table()?)?;
    exts.set(source, &entry)?;
    let ext_order: mlua::Table = registry.get("ext_order")?;
    ext_order.push(source)?;
    Ok(entry)
}

/// Create the registration table and build the `pi` API table. The table
/// is passed to each extension chunk as its single argument; it is *not*
/// installed as a global.
pub(crate) fn build(lua: &mlua::Lua) -> mlua::Result<mlua::Table> {
    let registry = lua.create_table()?;
    registry.set("events", lua.create_table()?)?;
    registry.set("exts", lua.create_table()?)?;
    registry.set("ext_order", lua.create_table()?)?;
    registry.set("source", "<host>")?;
    lua.set_named_registry_value(REGISTRY_KEY, registry)?;

    let pi = lua.create_table()?;

    let on = lua.create_function(|lua, (event, handler): (String, mlua::Function)| {
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
        Ok(())
    })?;
    pi.set("on", on)?;

    // Awaitable host future: the calling coroutine suspends; the VM thread
    // stays free to run the timer. Await time is excluded from the
    // watchdog budget (see vm.rs).
    let sleep = lua.create_async_function(|_lua, ms: u64| async move {
        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
        Ok(())
    })?;
    pi.set("sleep", sleep)?;

    // Spec `registerTool` (loader.ts): validate the mechanism-required
    // fields, then per-extension `Map.set` — re-registration of the same
    // name replaces the definition but keeps its position.
    let register_tool = lua.create_function(|lua, def: mlua::Table| {
        let name = def
            .get::<Option<String>>("name")?
            .filter(|n| !n.trim().is_empty())
            .ok_or_else(|| {
                mlua::Error::runtime("register_tool: tool.name must be a non-empty string")
            })?;
        if def.get::<Option<mlua::Function>>("execute")?.is_none() {
            return Err(mlua::Error::runtime(
                "register_tool: tool.execute must be a function",
            ));
        }
        let source = current_source(lua);
        let ext = ext_entry(lua, &source)?;
        let tools: mlua::Table = ext.get("tools")?;
        if tools.get::<Option<mlua::Value>>(name.as_str())?.is_none() {
            let order: mlua::Table = ext.get("tool_order")?;
            order.push(name.as_str())?;
        }
        tools.set(name.as_str(), def)?;
        Ok(())
    })?;
    pi.set("register_tool", register_tool)?;

    // Spec `registerCommand` (loader.ts): `{ name, ...options }` into the
    // per-extension map. Options are shallow-copied so later mutation of
    // the caller's table doesn't alias the registry.
    let register_command = lua.create_function(|lua, (name, options): (String, mlua::Table)| {
        if name.trim().is_empty() {
            return Err(mlua::Error::runtime(
                "register_command: name must be a non-empty string",
            ));
        }
        if options.get::<Option<mlua::Function>>("handler")?.is_none() {
            return Err(mlua::Error::runtime(
                "register_command: options.handler must be a function",
            ));
        }
        let source = current_source(lua);
        let ext = ext_entry(lua, &source)?;
        let entry = lua.create_table()?;
        for pair in options.pairs::<mlua::Value, mlua::Value>() {
            let (k, v) = pair?;
            entry.set(k, v)?;
        }
        entry.set("name", name.as_str())?;
        let commands: mlua::Table = ext.get("commands")?;
        if commands
            .get::<Option<mlua::Value>>(name.as_str())?
            .is_none()
        {
            let order: mlua::Table = ext.get("command_order")?;
            order.push(name.as_str())?;
        }
        commands.set(name.as_str(), entry)?;
        Ok(())
    })?;
    pi.set("register_command", register_command)?;

    Ok(pi)
}

/// Snapshot the handler list for `event` before dispatching so a handler
/// that subscribes new handlers mid-emit doesn't alter this dispatch.
pub(crate) fn event_handlers(
    lua: &mlua::Lua,
    event: &str,
) -> mlua::Result<Vec<(String, mlua::Function)>> {
    let events: mlua::Table = registry_table(lua)?.get("events")?;
    let Some(list) = events.get::<Option<mlua::Table>>(event)? else {
        return Ok(Vec::new());
    };
    let mut handlers = Vec::with_capacity(list.raw_len());
    for entry in list.sequence_values::<mlua::Table>() {
        let entry = entry?;
        let source: String = entry
            .get("source")
            .unwrap_or_else(|_| "<unknown>".to_owned());
        let handler: mlua::Function = entry.get("fn")?;
        handlers.push((source, handler));
    }
    Ok(handlers)
}

/// Flatten registrations of one kind: extension order (load order), then
/// per-extension insertion order — the iteration order of the spec's
/// nested `Map`s.
fn registrations(
    lua: &mlua::Lua,
    map_key: &str,
    order_key: &str,
) -> mlua::Result<Vec<(String, String, mlua::Table)>> {
    let registry = registry_table(lua)?;
    let exts: mlua::Table = registry.get("exts")?;
    let ext_order: mlua::Table = registry.get("ext_order")?;
    let mut out = Vec::new();
    for source in ext_order.sequence_values::<String>() {
        let source = source?;
        let Some(ext) = exts.get::<Option<mlua::Table>>(source.as_str())? else {
            continue;
        };
        let map: mlua::Table = ext.get(map_key)?;
        let order: mlua::Table = ext.get(order_key)?;
        for name in order.sequence_values::<String>() {
            let name = name?;
            if let Some(entry) = map.get::<Option<mlua::Table>>(name.as_str())? {
                out.push((source.clone(), name, entry));
            }
        }
    }
    Ok(out)
}

/// Spec `runner.getAllRegisteredTools()`: first registration per name
/// wins across extensions. Returns `(source, name, definition)`.
pub(crate) fn all_tools(lua: &mlua::Lua) -> mlua::Result<Vec<(String, String, mlua::Table)>> {
    let mut seen = HashSet::new();
    Ok(registrations(lua, "tools", "tool_order")?
        .into_iter()
        .filter(|(_, name, _)| seen.insert(name.clone()))
        .collect())
}

/// Spec `runner.getToolDefinition()`: the first extension that registered
/// the name.
pub(crate) fn find_tool(
    lua: &mlua::Lua,
    name: &str,
) -> mlua::Result<Option<(String, mlua::Table)>> {
    for (source, n, def) in all_tools(lua)? {
        if n == name {
            return Ok(Some((source, def)));
        }
    }
    Ok(None)
}

/// JSON metadata mirror of a tool definition: every field except
/// functions (`execute`, `prepare_arguments`) — handed to the host
/// uninterpreted.
pub(crate) fn tool_meta(def: &mlua::Table) -> mlua::Result<serde_json::Value> {
    let mut map = serde_json::Map::new();
    for pair in def.pairs::<mlua::Value, mlua::Value>() {
        let (k, v) = pair?;
        if matches!(v, mlua::Value::Function(_)) {
            continue;
        }
        let key = match k {
            mlua::Value::String(s) => s.to_str()?.to_owned(),
            mlua::Value::Integer(i) => i.to_string(),
            _ => continue,
        };
        map.insert(key, crate::convert::lua_to_json(v)?);
    }
    Ok(serde_json::Value::Object(map))
}

pub(crate) struct ResolvedCommand {
    pub(crate) source: String,
    pub(crate) name: String,
    pub(crate) invocation_name: String,
    pub(crate) description: Option<String>,
    pub(crate) handler: mlua::Function,
}

/// Spec `runner.resolveRegisteredCommands()`: flatten in extension order;
/// names registered by more than one extension get `name:occurrence`
/// invocation names, bumping the suffix past already-taken names.
pub(crate) fn resolved_commands(lua: &mlua::Lua) -> mlua::Result<Vec<ResolvedCommand>> {
    let regs = registrations(lua, "commands", "command_order")?;
    let mut counts: HashMap<String, usize> = HashMap::new();
    for (_, name, _) in &regs {
        *counts.entry(name.clone()).or_default() += 1;
    }
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut taken: HashSet<String> = HashSet::new();
    let mut out = Vec::with_capacity(regs.len());
    for (source, name, entry) in regs {
        let occurrence = seen.get(&name).copied().unwrap_or(0) + 1;
        seen.insert(name.clone(), occurrence);
        let mut invocation_name = if counts.get(&name).copied().unwrap_or(0) > 1 {
            format!("{name}:{occurrence}")
        } else {
            name.clone()
        };
        if taken.contains(&invocation_name) {
            let mut suffix = occurrence;
            loop {
                suffix += 1;
                invocation_name = format!("{name}:{suffix}");
                if !taken.contains(&invocation_name) {
                    break;
                }
            }
        }
        taken.insert(invocation_name.clone());
        let description = entry.get::<Option<String>>("description")?;
        let handler: mlua::Function = entry.get("handler")?;
        out.push(ResolvedCommand {
            source,
            name,
            invocation_name,
            description,
            handler,
        });
    }
    Ok(out)
}
