//! The Lua VM lives on one dedicated OS thread; the host talks to it over a
//! channel. Every dispatch into Lua runs under an instruction-hook watchdog
//! so a hung handler is killed instead of wedging the host (doctrine 02).

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};

use anyhow::{Result, anyhow};

use crate::convert::{json_to_lua, lua_to_json};
use crate::globals::registry_table;

/// Check the watchdog deadline every N VM instructions.
const NTH_INSTRUCTION: u32 = 1000;

/// Outcome of one handler dispatch, attributed to the extension that
/// registered it. The payload is the handler's return value serialized to
/// JSON (`None` when the handler returned nothing).
pub struct RawOutcome {
    pub source: String,
    pub result: Result<Option<String>, String>,
}

pub(crate) enum LuaMsg {
    /// Execute an extension chunk (registrations happen as side effects).
    Load {
        /// Chunk name for error messages and source attribution.
        source_key: String,
        source: String,
        timeout_ms: i64,
        reply: SyncSender<Result<()>>,
    },
    /// Call every handler subscribed to `event`, each under its own
    /// watchdog window; one hung or failing handler doesn't stop the rest.
    Emit {
        event: String,
        payload_json: String,
        timeout_ms: i64,
        reply: SyncSender<Vec<RawOutcome>>,
    },
    /// Invoke a registered command handler with a raw argument string.
    InvokeCommand {
        name: String,
        args: String,
        timeout_ms: i64,
        reply: SyncSender<Result<Option<String>>>,
    },
    /// Invoke a Lua-registered tool handler with JSON arguments. The reply
    /// is the tool's text content (a bare string stays a bare string; a
    /// table is JSON-encoded), `None` when the handler returned nothing.
    InvokeTool {
        name: String,
        args_json: String,
        timeout_ms: i64,
        reply: SyncSender<Result<Option<String>>>,
    },
}

/// Spawn the dedicated VM thread. With mlua's `send` feature `Lua` is
/// `Send`, so it moves into the thread without unsafe.
pub(crate) fn spawn(lua: mlua::Lua, rx: Receiver<LuaMsg>) {
    std::thread::Builder::new()
        .name("phi-host-lua".to_owned())
        .spawn(move || vm_loop(&lua, rx))
        .expect("spawn lua thread");
}

fn vm_loop(lua: &mlua::Lua, rx: Receiver<LuaMsg>) {
    // Watchdog deadline in unix-epoch ms; 0 = no deadline.
    let deadline = Arc::new(AtomicI64::new(0));

    while let Ok(msg) = rx.recv() {
        match msg {
            LuaMsg::Load {
                source_key,
                source,
                timeout_ms,
                reply,
            } => {
                let res = load_chunk(lua, &deadline, timeout_ms, &source_key, &source);
                let _ = reply.send(res);
            }
            LuaMsg::Emit {
                event,
                payload_json,
                timeout_ms,
                reply,
            } => {
                let outcomes = emit(lua, &deadline, timeout_ms, &event, &payload_json);
                let _ = reply.send(outcomes);
            }
            LuaMsg::InvokeCommand {
                name,
                args,
                timeout_ms,
                reply,
            } => {
                let res = run_with_timeout(lua, &deadline, timeout_ms, || {
                    invoke_command(lua, &name, &args)
                });
                let _ = reply.send(res);
            }
            LuaMsg::InvokeTool {
                name,
                args_json,
                timeout_ms,
                reply,
            } => {
                let res = run_with_timeout(lua, &deadline, timeout_ms, || {
                    invoke_tool(lua, &name, &args_json)
                });
                let _ = reply.send(res);
            }
        }
    }
}

fn load_chunk(
    lua: &mlua::Lua,
    deadline: &Arc<AtomicI64>,
    timeout_ms: i64,
    source_key: &str,
    source: &str,
) -> Result<()> {
    let registry = registry_table(lua)?;
    registry.set("source", source_key)?;
    let res = run_with_timeout(lua, deadline, timeout_ms, || {
        lua.load(source)
            .set_name(format!("@{source_key}"))
            .exec()
            .map_err(|e| anyhow!("{e}"))?;
        Ok(None)
    });
    let _ = registry.set("source", "<host>");
    res.map(|_| ())
}

fn emit(
    lua: &mlua::Lua,
    deadline: &Arc<AtomicI64>,
    timeout_ms: i64,
    event: &str,
    payload_json: &str,
) -> Vec<RawOutcome> {
    let handlers = match event_handlers(lua, event) {
        Ok(handlers) => handlers,
        Err(e) => {
            return vec![RawOutcome {
                source: "<host>".to_owned(),
                result: Err(format!("{e}")),
            }];
        }
    };

    let mut outcomes = Vec::with_capacity(handlers.len());
    for (source, handler) in handlers {
        // Registrations made from inside a handler attribute to its extension.
        if let Ok(registry) = registry_table(lua) {
            let _ = registry.set("source", source.as_str());
        }
        let result = run_with_timeout(lua, deadline, timeout_ms, || {
            let payload: serde_json::Value = serde_json::from_str(payload_json)?;
            let arg = json_to_lua(lua, &payload)?;
            let ret: mlua::Value = handler.call(arg)?;
            value_to_reply(ret)
        })
        .map_err(|e| format!("{e}"));
        outcomes.push(RawOutcome { source, result });
    }
    if let Ok(registry) = registry_table(lua) {
        let _ = registry.set("source", "<host>");
    }
    outcomes
}

/// Snapshot the handler list before dispatching so a handler that
/// subscribes new handlers mid-emit doesn't alter this dispatch.
fn event_handlers(lua: &mlua::Lua, event: &str) -> Result<Vec<(String, mlua::Function)>> {
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

fn invoke_command(lua: &mlua::Lua, name: &str, args: &str) -> Result<Option<String>> {
    let registry = registry_table(lua)?;
    let commands: mlua::Table = registry.get("commands")?;
    let entry: Option<mlua::Table> = commands.get(name)?;
    let entry = entry.ok_or_else(|| anyhow!("unknown command '{name}'"))?;
    let handler: mlua::Function = entry.get("handler")?;
    let source: String = entry
        .get("source")
        .unwrap_or_else(|_| "<unknown>".to_owned());
    let _ = registry.set("source", source);
    let ret: Result<mlua::Value> = handler.call(args).map_err(|e| anyhow!("{e}"));
    let _ = registry.set("source", "<host>");
    value_to_reply(ret?)
}

fn invoke_tool(lua: &mlua::Lua, name: &str, args_json: &str) -> Result<Option<String>> {
    let registry = registry_table(lua)?;
    let tools: mlua::Table = registry.get("tools")?;
    let entry: Option<mlua::Table> = tools.get(name)?;
    let entry = entry.ok_or_else(|| anyhow!("unknown tool '{name}'"))?;
    let handler: mlua::Function = entry.get("handler")?;
    let source: String = entry
        .get("source")
        .unwrap_or_else(|_| "<unknown>".to_owned());
    let _ = registry.set("source", source);
    let ret: Result<mlua::Value> = (|| {
        let args: serde_json::Value = serde_json::from_str(args_json)?;
        let arg = json_to_lua(lua, &args)?;
        handler.call(arg).map_err(|e| anyhow!("{e}"))
    })();
    let _ = registry.set("source", "<host>");
    // Unlike commands, a bare-string tool result is the content itself —
    // it goes back to the model verbatim, not into a `{ message }` wrapper.
    match ret? {
        mlua::Value::Nil => Ok(None),
        mlua::Value::String(s) => Ok(Some(s.to_str()?.to_owned())),
        other => Ok(Some(serde_json::to_string(&lua_to_json(other)?)?)),
    }
}

/// Serialize a handler return value for the reply channel. `nil` → `None`;
/// a bare string is shorthand for `{ message = ... }`; anything else is
/// converted as-is and handed to the caller uninterpreted — the host never
/// matches on result variants (DESIGN.md: no closed enums at the seam).
fn value_to_reply(val: mlua::Value) -> Result<Option<String>> {
    match val {
        mlua::Value::Nil => Ok(None),
        mlua::Value::String(s) => Ok(Some(serde_json::to_string(
            &serde_json::json!({ "message": s.to_str()?.to_owned() }),
        )?)),
        other => {
            let json = lua_to_json(other)?;
            Ok(Some(serde_json::to_string(&json)?))
        }
    }
}

/// Run one Lua call bounded by the watchdog: set a shared deadline, install
/// an instruction-counting hook that errors past the deadline, and always
/// remove the hook and clear the deadline afterwards. A pure-Lua busy loop
/// is killed reliably; a blocking C call cannot be interrupted by the hook
/// (the caller's reply timeout is the backstop for that case).
fn run_with_timeout<F>(
    lua: &mlua::Lua,
    deadline: &Arc<AtomicI64>,
    timeout_ms: i64,
    f: F,
) -> Result<Option<String>>
where
    F: FnOnce() -> Result<Option<String>>,
{
    let now_ms = unix_ms();
    deadline.store(now_ms.saturating_add(timeout_ms), Ordering::Relaxed);

    let triggers = mlua::HookTriggers::new().every_nth_instruction(NTH_INSTRUCTION);
    let deadline_hook = Arc::clone(deadline);
    lua.set_hook(triggers, move |_lua, _debug| {
        if unix_ms() >= deadline_hook.load(Ordering::Relaxed) {
            return Err(mlua::Error::runtime(format!(
                "handler timed out (watchdog, {timeout_ms}ms)"
            )));
        }
        Ok(mlua::VmState::Continue)
    })?;

    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));

    lua.remove_hook();
    deadline.store(0, Ordering::Relaxed);

    res.unwrap_or_else(|p| std::panic::resume_unwind(p))
}

fn unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
