use anyhow::Result;

use super::bridge::{call_lua_command, call_lua_event};

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
}

// ---------------------------------------------------------------------------
// Lua global setup
// ---------------------------------------------------------------------------

struct SendableLua(mlua::Lua);

// SAFETY: See the invariant above. The Rc inside mlua::Lua is never shared
// across thread boundaries; `SendableLua` enforces single-threaded ownership
// by construction — it is created once, immediately moved into the spawned
// thread, and never cloned.
unsafe impl Send for SendableLua {}

pub(super) fn spawn_lua_thread(lua: mlua::Lua, rx: std::sync::mpsc::Receiver<LuaMsg>) {
    let sendable = SendableLua(lua);
    std::thread::spawn(move || {
        lua_thread_loop(sendable, rx);
    });
}

// ---------------------------------------------------------------------------
// Lua thread loop
// ---------------------------------------------------------------------------

fn lua_thread_loop(lua: SendableLua, rx: std::sync::mpsc::Receiver<LuaMsg>) {
    let lua = &lua.0;
    loop {
        match rx.recv() {
            Ok(LuaMsg::CallEvent {
                event_name,
                payload_json,
                reply,
            }) => {
                let _ = reply.send(call_lua_event(lua, &event_name, &payload_json));
            }
            Ok(LuaMsg::InvokeCommand { name, args, reply }) => {
                let _ = reply.send(call_lua_command(lua, &name, &args));
            }
            Err(_) => break,
        }
    }
}

// ---------------------------------------------------------------------------
// Call helpers
// ---------------------------------------------------------------------------
