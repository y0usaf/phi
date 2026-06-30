use anyhow::Result;

use super::bridge::{call_lua_command, call_lua_event, call_lua_tool};

// ---------------------------------------------------------------------------
// Watchdog & Timeout Configuration
// ---------------------------------------------------------------------------

/// Check the deadline every N VM instructions.
const NTH_INSTRUCTION: u32 = 1000;

/// Default timeout in milliseconds.
const DEFAULT_TIMEOUT_MS: i64 = 5000;

// ---------------------------------------------------------------------------
// Lua thread communication
// ---------------------------------------------------------------------------

pub enum LuaMsg {
    CallEvent {
        event_name: String,
        payload_json: String,
        reply: std::sync::mpsc::SyncSender<Result<Option<String>>>,
    },
    InvokeCommand {
        name: String,
        args: String,
        reply: std::sync::mpsc::SyncSender<Result<Option<String>>>,
    },
    InvokeTool {
        name: String,
        input: String,
        reply: std::sync::mpsc::SyncSender<Result<Option<String>>>,
    },
    RenderOverlay {
        name: String,
        reply: std::sync::mpsc::SyncSender<Result<Option<String>>>,
    },
}

/// Spawn a dedicated OS thread that owns the Lua VM.
///
/// With mlua's `send` feature, `mlua::Lua` is `Send`, so it can be moved
/// safely into the spawned thread without any `unsafe` wrapper.
pub(super) fn spawn_lua_thread(lua: mlua::Lua, rx: std::sync::mpsc::Receiver<LuaMsg>) {
    std::thread::spawn(move || {
        lua_thread_loop(&lua, rx);
    });
}

// ---------------------------------------------------------------------------
// Lua thread loop
// ---------------------------------------------------------------------------

fn lua_thread_loop(lua: &mlua::Lua, rx: std::sync::mpsc::Receiver<LuaMsg>) {
    // Allocate an atomic deadline variable for the watchdog hook.
    // A value of 0 implies no deadline or cleared timeout.
    let deadline = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0));

    loop {
        match rx.recv() {
            Ok(LuaMsg::CallEvent {
                event_name,
                payload_json,
                reply,
            }) => {
                let res = run_with_timeout(lua, &deadline, DEFAULT_TIMEOUT_MS, || {
                    call_lua_event(lua, &event_name, &payload_json)
                });
                let _ = reply.send(res);
            }
            Ok(LuaMsg::InvokeCommand { name, args, reply }) => {
                let res = run_with_timeout(lua, &deadline, DEFAULT_TIMEOUT_MS, || {
                    call_lua_command(lua, &name, &args)
                });
                let _ = reply.send(res);
            }
            Ok(LuaMsg::InvokeTool { name, input, reply }) => {
                let res = run_with_timeout(lua, &deadline, DEFAULT_TIMEOUT_MS, || {
                    call_lua_tool(lua, &name, &input)
                });
                let _ = reply.send(res);
            }
            Ok(LuaMsg::RenderOverlay { name, reply }) => {
                let res = run_with_timeout(lua, &deadline, DEFAULT_TIMEOUT_MS, || {
                    super::bridge::call_lua_overlay_render(lua, &name)
                });
                let _ = reply.send(res);
            }
            Err(_) => break,
        }
    }
}

// ---------------------------------------------------------------------------
// Call helpers
// ---------------------------------------------------------------------------

/// Executes a Lua call with a watchdog timeout.
///
/// This sets a shared deadline, installs an instruction-counting hook to check
/// the deadline, runs the call, and then guarantees the hook is removed and
/// deadline cleared, even if a timeout or error occurs.
fn run_with_timeout<F>(
    lua: &mlua::Lua,
    deadline: &std::sync::Arc<std::sync::atomic::AtomicI64>,
    timeout_ms: i64,
    f: F,
) -> Result<Option<String>>
where
    F: FnOnce() -> Result<Option<String>>,
{
    // Calculate and set the new deadline
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let new_deadline = now_ms.saturating_add(timeout_ms);
    deadline.store(new_deadline, std::sync::atomic::Ordering::Relaxed);

    // Install the instruction hook
    let hook_triggers = mlua::HookTriggers::new().every_nth_instruction(NTH_INSTRUCTION);
    let deadline_clone = std::sync::Arc::clone(deadline);

    lua.set_hook(hook_triggers, move |_lua, _debug| {
        let current_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        if current_time >= deadline_clone.load(std::sync::atomic::Ordering::Relaxed) {
            // Returning an error from the hook aborts the Lua execution and
            // causes the `f()` call to return an `mlua::Error`.
            return Err(mlua::Error::external(anyhow::anyhow!(
                "Lua execution timed out"
            )));
        }
        Ok(mlua::VmState::Continue)
    })?;

    // Execute the closure containing the Lua call.
    // Ensure hook removal and deadline clearing regardless of the outcome.
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));

    // Remove hook and clear deadline whether the call succeeded, errored, or panicked.
    lua.remove_hook();
    deadline.store(0, std::sync::atomic::Ordering::Relaxed);

    // Propagate panic if it occurred, otherwise process the result.
    let res = res.unwrap_or_else(|p| std::panic::resume_unwind(p));

    // Bubble up timeout or execution errors as anyhow::Error
    res
}
