//! The `pi` API table — the canonical extension surface, named after the
//! spec's `ExtensionAPI` parameter so example translations stay literal
//! (`export default function (pi) { ... }` → `local pi = ...`).
//!
//! Registration state lives Lua-side in a registry-held table (handlers
//! stay Lua functions); the host reads it at dispatch time. The table is
//! kept in the named registry, not a global, so extension code can't
//! clobber it by accident.
//!
//! Surface at WS1.1 (grown only alongside an exercising example):
//! - `pi.on(event, fn)` — open string vocabulary, no closed enum.
//! - `pi.sleep(ms)` — first awaitable host future: suspends the handler
//!   coroutine without burning watchdog budget (superset of pi's
//!   `await new Promise(setTimeout)`; divergence 3).

/// Key of the registration table in the Lua named registry.
pub(crate) const REGISTRY_KEY: &str = "phi-host";

/// The registration table: `{ events = {}, source = "<host>" }`.
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

/// Create the registration table and build the `pi` API table. The table
/// is passed to each extension chunk as its single argument; it is *not*
/// installed as a global.
pub(crate) fn build(lua: &mlua::Lua) -> mlua::Result<mlua::Table> {
    let registry = lua.create_table()?;
    registry.set("events", lua.create_table()?)?;
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
