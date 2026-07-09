//! The `pi` API table — the canonical extension surface, named after the
//! spec's `ExtensionAPI` parameter so example translations stay literal
//! (`export default function (pi) { ... }` → `local pi = ...`).
//!
//! Registration state lives Lua-side in a registry-held table (handlers
//! stay Lua functions); the host reads it at dispatch time. The table is
//! kept in the named registry, not a global, so extension code can't
//! clobber it by accident.
//!
//! Surface at WS1.3 (grown only alongside an exercising example):
//! - `pi.on(event, fn)` — open string vocabulary, no closed enum.
//! - `pi.sleep(ms)` — first awaitable host future: suspends the handler
//!   coroutine without burning watchdog budget (superset of pi's
//!   `await new Promise(setTimeout)`; divergence 3).
//! - `pi.parallel(tasks)` — structured concurrency for awaitable Lua callbacks;
//!   completion-order outcomes let Lua policy reproduce Promise semantics.
//! - `pi.register_tool(def)` — spec `registerTool` (`loader.ts`):
//!   per-extension map, re-registration replaces in place.
//! - `pi.register_command(name, opts)` — spec `registerCommand`
//!   (`loader.ts`): `{ name, ...opts }` into the per-extension map.
//! - `pi.register_provider(name, config)` / `pi.unregister_provider(name)`
//!   — spec `registerProvider`/`unregisterProvider`: registrations are
//!   queued host-side (the spec's initial-load behavior — the runner
//!   applies them once bound); re-registration merges defined keys over
//!   the stored config (spec `upsertRegisteredProvider`). Application to
//!   the model registry is the embedder's (phi-app, WS2.6).
//! - `pi.exec(command, args?, options?)` — spec `ExtensionAPI.exec`
//!   (`exec.rs` ← `core/exec.ts`).
//! - `pi.fs` / `pi.path` / `pi.env` / `pi.cwd()` — OS bindings (`os.rs`;
//!   ambient Node in the spec, explicit bindings under divergence 1).

use mlua::{UserData, UserDataMethods};
use std::collections::{HashMap, HashSet};

struct LuaSelectList(phi_tui::select_list::SelectList);
impl UserData for LuaSelectList {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method_mut("filter", |_, this, filter: String| {
            this.0.set_filter(&filter);
            Ok(())
        });
        methods.add_method("selected", |lua, this, ()| {
            this.0
                .selected()
                .map(|item| {
                    let table = lua.create_table()?;
                    table.set("value", item.value.clone())?;
                    table.set("label", item.label.clone())?;
                    table.set("description", item.description.clone())?;
                    Ok(table)
                })
                .transpose()
        });
        methods.add_method("render", |lua, this, width: usize| {
            let result = lua.create_table()?;
            for line in this.0.render(width) {
                result.push(line)?;
            }
            Ok(result)
        });
        methods.add_method_mut("input", |_, this, data: String| Ok(this.0.handle(&data)))
    }
}

struct LuaEditor(phi_tui::editor::Editor);
impl UserData for LuaEditor {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("value", |_, this, ()| Ok(this.0.value().to_owned()));
        methods.add_method("cursor", |_, this, ()| Ok(this.0.cursor()));
        methods.add_method_mut("input", |_, this, data: String| {
            Ok(this.0.handle(&data).is_some())
        });
        methods.add_method_mut("insert", |_, this, text: String| {
            this.0.insert(&text);
            Ok(())
        });
        methods.add_method_mut("backspace", |_, this, ()| {
            this.0.backspace();
            Ok(())
        });
        methods.add_method_mut("delete", |_, this, ()| {
            this.0.delete();
            Ok(())
        });
        methods.add_method_mut("undo", |_, this, ()| {
            this.0.undo();
            Ok(())
        });
        methods.add_method_mut("yank", |_, this, ()| {
            this.0.yank();
            Ok(())
        });
        methods.add_method_mut("yank_pop", |_, this, ()| {
            this.0.yank_pop();
            Ok(())
        });
        methods.add_method_mut("kill_to_start", |_, this, ()| {
            this.0.kill_to_start();
            Ok(())
        });
        methods.add_method_mut("kill_to_end", |_, this, ()| {
            this.0.kill_to_end();
            Ok(())
        });
        methods.add_method_mut("word_left", |_, this, ()| {
            this.0.move_word_left();
            Ok(())
        });
        methods.add_method_mut("word_right", |_, this, ()| {
            this.0.move_word_right();
            Ok(())
        });
    }
}

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
    entry.set("providers", lua.create_table()?)?;
    entry.set("provider_order", lua.create_table()?)?;
    exts.set(source, &entry)?;
    let ext_order: mlua::Table = registry.get("ext_order")?;
    ext_order.push(source)?;
    Ok(entry)
}

/// Create the registration table and build the `pi` API table. The table
/// is passed to each extension chunk as its single argument; it is *not*
/// installed as a global. `cwd` is the host working directory (spec: the
/// loader's injected `cwd`) — the `pi.exec` default and `pi.cwd()`.
pub(crate) fn build(lua: &mlua::Lua, cwd: &str) -> mlua::Result<mlua::Table> {
    let registry = lua.create_table()?;
    registry.set("events", lua.create_table()?)?;
    registry.set("exts", lua.create_table()?)?;
    registry.set("ext_order", lua.create_table()?)?;
    registry.set("source", "<host>")?;
    lua.set_named_registry_value(REGISTRY_KEY, registry)?;

    let pi = lua.create_table()?;

    // JavaScript String.prototype.normalize mechanism. Lua 5.4 has no
    // Unicode normalization; product policy (which form and when) stays
    // in Lua. Exercised by examples/extensions/os-demo.lua.
    let text = lua.create_table()?;
    text.set(
        "nfkc",
        lua.create_function(|_, value: String| {
            use unicode_normalization::UnicodeNormalization;
            Ok(value.nfkc().collect::<String>())
        })?,
    )?;
    pi.set("text", text)?;

    // JSON.parse mechanism for model argument recovery (some providers
    // stringify structured arguments). Validation and fallback stay Lua.
    let json = lua.create_table()?;
    json.set(
        "decode",
        lua.create_function(|lua, value: String| {
            let parsed: serde_json::Value = serde_json::from_str(&value)
                .map_err(|error| mlua::Error::runtime(error.to_string()))?;
            crate::convert::json_to_lua(lua, &parsed)
        })?,
    )?;
    pi.set("json", json)?;

    // Mechanisms used by the Lua-authored agent loop. Definitions stay in
    // Lua (including execute/prepare functions); this returns the resolved
    // first-registration-wins view rather than a JSON metadata mirror.
    pi.set(
        "registered_tools",
        lua.create_function(|lua, ()| {
            let result = lua.create_table()?;
            for (_, _, def) in all_tools(lua)? {
                result.push(def)?;
            }
            Ok(result)
        })?,
    )?;
    pi.set(
        "validate_tool_arguments",
        lua.create_function(
            |lua, (name, schema, arguments): (String, mlua::Value, mlua::Value)| {
                let schema = crate::convert::lua_to_json(schema)?;
                let arguments = crate::convert::lua_to_json(arguments)?;
                let validated = crate::schema::validate_tool_arguments(&name, &schema, &arguments)
                    .map_err(|error| mlua::Error::runtime(error.to_string()))?;
                crate::convert::json_to_lua(lua, &validated)
            },
        )?,
    )?;

    // Session persistence is a mechanism binding; Lua owns the policy of which
    // standard messages to append (the agent pack subscribes to message_end).
    let session_append = lua.create_function(|_, (path, message): (String, mlua::Value)| {
        let message = crate::convert::lua_to_json(message)?;
        let cwd = std::env::current_dir().map_err(mlua::Error::external)?;
        let agent_dir = crate::discover::agent_dir();
        let mut manager = if std::path::Path::new(&path).exists() {
            phi_session::SessionManager::open(&path, None, Some(&cwd.to_string_lossy()), &agent_dir)
                .map_err(|e| mlua::Error::external(e.to_string()))?
        } else {
            phi_session::SessionManager::open(&path, None, Some(&cwd.to_string_lossy()), &agent_dir)
                .map_err(|e| mlua::Error::external(e.to_string()))?
        };
        manager
            .append_message(message)
            .map_err(|e| mlua::Error::external(e.to_string()))?;
        manager
            .flush()
            .map_err(|e| mlua::Error::external(e.to_string()))?;
        Ok(())
    })?;
    pi.set("session_append", session_append)?;
    // Thin CLI output mechanism; formatting and event policy remain Lua.
    pi.set(
        "output",
        lua.create_function(|_, text: String| {
            use std::io::Write as _;
            print!("{text}");
            std::io::stdout().flush().map_err(mlua::Error::external)
        })?,
    )?;

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
    // Generic structured concurrency mechanism. Policy (which tasks to group,
    // and how to order their results) remains in Lua. Results are reported in
    // completion order so callers can reproduce Promise completion semantics.
    let parallel = lua.create_async_function(|lua, tasks: mlua::Table| async move {
        use futures_util::stream::{FuturesUnordered, StreamExt};

        let pending = FuturesUnordered::new();
        for (offset, task) in tasks.sequence_values::<mlua::Function>().enumerate() {
            let task = task?;
            pending.push(async move {
                let result = task.call_async::<mlua::Value>(()).await;
                (offset + 1, result)
            });
        }
        let completed = lua.create_table()?;
        let mut pending = pending;
        while let Some((index, result)) = pending.next().await {
            let entry = lua.create_table()?;
            entry.set("index", index)?;
            match result {
                Ok(value) => {
                    entry.set("ok", true)?;
                    entry.set("value", value)?;
                }
                Err(error) => {
                    entry.set("ok", false)?;
                    entry.set("error", error.to_string())?;
                }
            }
            completed.push(entry)?;
        }
        Ok(completed)
    })?;
    pi.set("parallel", parallel)?;
    let epoch = std::time::Instant::now();
    pi.set(
        "monotonic_ms",
        lua.create_function(move |_, ()| {
            Ok(u64::try_from(epoch.elapsed().as_millis()).unwrap_or(u64::MAX))
        })?,
    )?;

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

    // Spec `registerProvider` (loader.ts / model-registry.ts): the
    // host-side half — store the config per extension, merging defined
    // keys over an existing registration of the same name (spec
    // `upsertRegisteredProvider`). Function values (`streamSimple`,
    // `oauth.*`) stay Lua-side, invocable when their mechanisms land
    // (WS2.5 auth, WS5 custom streams); the JSON mirror strips them.
    let register_provider = lua.create_function(|lua, (name, config): (String, mlua::Table)| {
        if name.trim().is_empty() {
            return Err(mlua::Error::runtime(
                "register_provider: name must be a non-empty string",
            ));
        }
        let source = current_source(lua);
        let ext = ext_entry(lua, &source)?;
        let providers: mlua::Table = ext.get("providers")?;
        let entry = match providers.get::<Option<mlua::Table>>(name.as_str())? {
            Some(existing) => existing,
            None => {
                let order: mlua::Table = ext.get("provider_order")?;
                order.push(name.as_str())?;
                let entry = lua.create_table()?;
                providers.set(name.as_str(), &entry)?;
                entry
            }
        };
        for pair in config.pairs::<mlua::Value, mlua::Value>() {
            let (k, v) = pair?;
            entry.set(k, v)?;
        }
        Ok(())
    })?;
    pi.set("register_provider", register_provider)?;

    // Spec `unregisterProvider`: removal by name regardless of which
    // extension registered it (the spec's registry is keyed globally);
    // no effect if the name was never registered.
    let unregister_provider = lua.create_function(|lua, name: String| {
        let registry = registry_table(lua)?;
        let exts: mlua::Table = registry.get("exts")?;
        let ext_order: mlua::Table = registry.get("ext_order")?;
        for source in ext_order.sequence_values::<String>() {
            let source = source?;
            let Some(ext) = exts.get::<Option<mlua::Table>>(source.as_str())? else {
                continue;
            };
            let providers: mlua::Table = ext.get("providers")?;
            if providers
                .get::<Option<mlua::Value>>(name.as_str())?
                .is_none()
            {
                continue;
            }
            providers.set(name.as_str(), mlua::Value::Nil)?;
            let order: mlua::Table = ext.get("provider_order")?;
            let kept = lua.create_table()?;
            for entry in order.sequence_values::<String>() {
                let entry = entry?;
                if entry != name {
                    kept.push(entry)?;
                }
            }
            ext.set("provider_order", kept)?;
        }
        Ok(())
    })?;
    pi.set("unregister_provider", unregister_provider)?;

    // TUI mechanism bindings. Components and differential cell output remain
    // policy-free Rust mechanism; Lua owns composition and frontend behavior.
    let tui = lua.create_table()?;
    tui.set("text_render", lua.create_function(|lua, (text, width, padding_x, padding_y): (String, usize, Option<usize>, Option<usize>)| {
        let lines = phi_tui::component::render_text(&text, width, padding_x.unwrap_or(1), padding_y.unwrap_or(1));
        let result = lua.create_table()?;
        for line in lines { result.push(line)?; }
        Ok(result)
    })?)?;
    tui.set(
        "differential_render",
        lua.create_function(
            |_, (previous, lines, clear): (mlua::Table, mlua::Table, Option<bool>)| {
                let previous: Vec<String> =
                    previous.sequence_values().collect::<mlua::Result<_>>()?;
                let lines: Vec<String> = lines.sequence_values().collect::<mlua::Result<_>>()?;
                Ok(phi_tui::component::differential_render(
                    &previous,
                    &lines,
                    clear.unwrap_or(false),
                ))
            },
        )?,
    )?;
    tui.set(
        "editor",
        lua.create_function(|lua, value: Option<String>| {
            lua.create_userdata(LuaEditor(phi_tui::editor::Editor::new(
                value.unwrap_or_default(),
            )))
        })?,
    )?;
    tui.set(
        "select_list",
        lua.create_function(|lua, (items, max_visible): (mlua::Table, Option<usize>)| {
            let mut values = Vec::new();
            for item in items.sequence_values::<mlua::Table>() {
                let item = item?;
                values.push(phi_tui::select_list::SelectItem {
                    value: item.get("value")?,
                    label: item.get("label")?,
                    description: item.get("description")?,
                });
            }
            lua.create_userdata(LuaSelectList(phi_tui::select_list::SelectList::new(
                values,
                max_visible.unwrap_or(5),
            )))
        })?,
    )?;
    tui.set(
        "decode_key",
        lua.create_function(|_, data: String| Ok(phi_tui::editor::decode_key(&data)))?,
    )?;
    tui.set(
        "decode_printable",
        lua.create_function(|_, data: String| Ok(phi_tui::editor::decode_printable(&data)))?,
    )?;
    pi.set("tui", tui)?;

    crate::ai::install(lua, &pi)?;
    crate::exec::install(lua, &pi, cwd)?;
    crate::os::install(lua, &pi, cwd)?;

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

/// All provider registrations in extension load order, then
/// per-extension registration order (spec: the runner drains queued
/// `registerProvider` calls in order; the consumer applies the spec's
/// global upsert). Returns `(source, name, config)`.
pub(crate) fn all_providers(lua: &mlua::Lua) -> mlua::Result<Vec<(String, String, mlua::Table)>> {
    registrations(lua, "providers", "provider_order")
}

/// JSON mirror of a provider config: function values are stripped at
/// any depth (functions never cross the bridge — `streamSimple` and the
/// `oauth` callbacks stay Lua-side for later invocation; non-function
/// fields like `oauth.name` survive).
pub(crate) fn provider_meta(config: &mlua::Table) -> mlua::Result<serde_json::Value> {
    let mut map = serde_json::Map::new();
    for pair in config.pairs::<mlua::Value, mlua::Value>() {
        let (k, v) = pair?;
        if matches!(v, mlua::Value::Function(_)) {
            continue;
        }
        let key = match k {
            mlua::Value::String(s) => s.to_str()?.to_owned(),
            mlua::Value::Integer(i) => i.to_string(),
            _ => continue,
        };
        let value = match v {
            mlua::Value::Table(t) => provider_table_value(&t)?,
            other => crate::convert::lua_to_json(other)?,
        };
        map.insert(key, value);
    }
    Ok(serde_json::Value::Object(map))
}

/// [`provider_meta`] recursion: array tables keep their shape (function
/// entries would shift positions, so they become `null`); map tables
/// drop function values.
fn provider_table_value(t: &mlua::Table) -> mlua::Result<serde_json::Value> {
    let len = t.raw_len();
    let mut is_array = len > 0;
    if is_array {
        let mut count = 0;
        for pair in t.pairs::<mlua::Value, mlua::Value>() {
            let (k, _) = pair?;
            match k {
                mlua::Value::Integer(i) if i >= 1 && (i as usize) <= len => count += 1,
                _ => {
                    is_array = false;
                    break;
                }
            }
        }
        if is_array {
            is_array = count == len;
        }
    }
    if is_array {
        let mut arr = Vec::with_capacity(len);
        for i in 1..=len {
            let v: mlua::Value = t.get(i)?;
            arr.push(match v {
                mlua::Value::Function(_) => serde_json::Value::Null,
                mlua::Value::Table(inner) => provider_table_value(&inner)?,
                other => crate::convert::lua_to_json(other)?,
            });
        }
        return Ok(serde_json::Value::Array(arr));
    }
    provider_meta(t)
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
