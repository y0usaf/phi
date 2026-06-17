use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use phi_ext::{
    CommandInvocation, CommandOutput, CommandSpec, EventHandlerRegistration, EventKind,
    EventReturn, ExtensionHost, ExtensionManifest, InputActionKind, LifecycleEvent,
};

pub struct LuaExtension {
    path: PathBuf,
    source: String,
}

impl LuaExtension {
    pub fn from_file(path: &Path) -> Result<Self> {
        Ok(Self {
            path: path.to_owned(),
            source: std::fs::read_to_string(path)?,
        })
    }
}

// ---------------------------------------------------------------------------
// Channel message type
// ---------------------------------------------------------------------------

enum LuaMsg {
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

fn setup_phi_global(lua: &mlua::Lua) -> mlua::Result<()> {
    let globals = lua.globals();

    // Empty tables used by phi.*  methods
    let phi_handlers = lua.create_table()?;
    let phi_registered_events = lua.create_table()?;
    let phi_commands = lua.create_table()?;
    let phi_registered_commands = lua.create_table()?;
    let phi_notifications = lua.create_table()?;

    globals.set("_phi_handlers", phi_handlers)?;
    globals.set("_phi_registered_events", phi_registered_events)?;
    globals.set("_phi_commands", phi_commands)?;
    globals.set("_phi_registered_commands", phi_registered_commands)?;
    globals.set("_phi_notifications", phi_notifications)?;

    // Build the phi table
    let phi = lua.create_table()?;

    // phi.on(event_name, handler)
    let on_fn = lua.create_function(|lua, (event_name, handler): (String, mlua::Function)| {
        let globals = lua.globals();
        let handlers: mlua::Table = globals.get("_phi_handlers")?;
        handlers.set(event_name.clone(), handler)?;
        let reg: mlua::Table = globals.get("_phi_registered_events")?;
        let len = reg.raw_len();
        reg.raw_set(len + 1, event_name)?;
        Ok(())
    })?;
    phi.set("on", on_fn)?;

    // phi.register_command(name, opts)
    let register_command_fn =
        lua.create_function(|lua, (name, opts): (String, mlua::Table)| {
            let globals = lua.globals();
            let commands: mlua::Table = globals.get("_phi_commands")?;
            commands.set(name.clone(), opts)?;
            let reg: mlua::Table = globals.get("_phi_registered_commands")?;
            let len = reg.raw_len();
            reg.raw_set(len + 1, name)?;
            Ok(())
        })?;
    phi.set("register_command", register_command_fn)?;

    // phi.notify(msg)
    let notify_fn = lua.create_function(|lua, msg: String| {
        let globals = lua.globals();
        let notifications: mlua::Table = globals.get("_phi_notifications")?;
        let len = notifications.raw_len();
        notifications.raw_set(len + 1, msg)?;
        Ok(())
    })?;
    phi.set("notify", notify_fn)?;

    // phi.set_status(key, text) -- no-op
    let set_status_fn = lua.create_function(|_, (_key, _text): (mlua::Value, mlua::Value)| Ok(()))?;
    phi.set("set_status", set_status_fn)?;

    // phi.clear_status(key) -- no-op
    let clear_status_fn = lua.create_function(|_, _key: mlua::Value| Ok(()))?;
    phi.set("clear_status", clear_status_fn)?;

    globals.set("phi", phi)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Extension impl
// ---------------------------------------------------------------------------

impl phi_ext::Extension for LuaExtension {
    fn manifest(&self) -> ExtensionManifest {
        let stem = self
            .path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");
        ExtensionManifest {
            id: format!("lua:{stem}"),
            name: stem.to_owned(),
            version: "0.1.0".to_owned(),
            description: format!("Lua extension: {}", self.path.display()),
        }
    }

    fn register(&self, host: &mut dyn ExtensionHost) -> Result<()> {
        let lua = mlua::Lua::new();
        setup_phi_global(&lua)?;

        let filename = self
            .path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown.lua")
            .to_owned();

        lua.load(&self.source).set_name(&filename).exec()?;

        // Read registered events
        let globals = lua.globals();
        let reg_events: mlua::Table = globals.get("_phi_registered_events")?;
        let event_names: Vec<String> = (1..)
            .map_while(|i| {
                reg_events
                    .get::<Option<String>>(i)
                    .ok()
                    .flatten()
            })
            .collect();

        // Read registered commands
        let reg_commands: mlua::Table = globals.get("_phi_registered_commands")?;
        let command_names: Vec<String> = (1..)
            .map_while(|i| {
                reg_commands
                    .get::<Option<String>>(i)
                    .ok()
                    .flatten()
            })
            .collect();

        // Read command descriptions
        let phi_commands_table: mlua::Table = globals.get("_phi_commands")?;
        let mut commands: Vec<(String, String)> = Vec::new();
        for name in &command_names {
            let cmd_table: mlua::Table = phi_commands_table.get(name.as_str())?;
            let description: String = cmd_table.get("description").unwrap_or_default();
            commands.push((name.clone(), description));
        }

        // Create the channel
        let (tx, rx) = std::sync::mpsc::sync_channel::<LuaMsg>(32);

        // Spawn the dedicated Lua owner thread.
        // SAFETY: mlua::Lua without the "send" feature is !Send due to
        // internal Rc. We guarantee sole ownership from this point: the Lua
        // VM is moved into SendableLua (which declares Send) and from there
        // exclusively into the spawned thread. All Lua calls are serialized
        // through the sync_channel; no other thread ever touches the VM.
        spawn_lua_thread(lua, rx);

        let tx = Arc::new(tx);

        // Register event handlers
        for event_name in event_names {
            let kind = parse_event_kind(&event_name)?;
            let tx2 = Arc::clone(&tx);
            let name2 = event_name.clone();
            host.subscribe(EventHandlerRegistration {
                event: kind,
                label: format!("lua:{}:{}", self.path.display(), event_name),
                handler: Arc::new(move |event: LifecycleEvent| {
                    let payload_json = serde_json::to_string(&event.payload)?;
                    let (rtx, rrx) = std::sync::mpsc::sync_channel(1);
                    tx2.send(LuaMsg::CallEvent {
                        event_name: name2.clone(),
                        payload_json,
                        reply: rtx,
                    })
                    .map_err(|e| anyhow::anyhow!("lua send: {e}"))?;
                    let json_opt = rrx
                        .recv()
                        .map_err(|e| anyhow::anyhow!("lua recv: {e}"))??;
                    decode_event_return(json_opt)
                }),
            })?;
        }

        // Register commands
        for (cmd_name, description) in commands {
            let tx3 = Arc::clone(&tx);
            let name3 = cmd_name.clone();
            host.register_command(CommandSpec {
                name: cmd_name.clone(),
                description,
                handler: Arc::new(move |inv: CommandInvocation| {
                    let (rtx, rrx) = std::sync::mpsc::sync_channel(1);
                    tx3.send(LuaMsg::InvokeCommand {
                        name: name3.clone(),
                        args: inv.raw_args,
                        reply: rtx,
                    })
                    .map_err(|e| anyhow::anyhow!("lua send: {e}"))?;
                    let msg_opt = rrx
                        .recv()
                        .map_err(|e| anyhow::anyhow!("lua recv: {e}"))??;
                    Ok(CommandOutput { message: msg_opt })
                }),
            })?;
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Lua thread helpers
// ---------------------------------------------------------------------------

/// Newtype that declares mlua::Lua as Send.
///
/// SAFETY invariant: exactly one thread owns the wrapped Lua at any time.
/// The public API of this module only ever moves it into `lua_thread_loop`,
/// where it lives for its entire remaining lifetime.
struct SendableLua(mlua::Lua);

// SAFETY: See the invariant above. The Rc inside mlua::Lua is never shared
// across thread boundaries; `SendableLua` enforces single-threaded ownership
// by construction — it is created once, immediately moved into the spawned
// thread, and never cloned.
unsafe impl Send for SendableLua {}

fn spawn_lua_thread(lua: mlua::Lua, rx: std::sync::mpsc::Receiver<LuaMsg>) {
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

fn call_lua_event(lua: &mlua::Lua, event_name: &str, payload_json: &str) -> Result<Option<String>> {
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

fn call_lua_command(lua: &mlua::Lua, name: &str, args: &str) -> Result<Option<String>> {
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

// ---------------------------------------------------------------------------
// JSON <-> Lua conversion
// ---------------------------------------------------------------------------

fn json_to_lua(lua: &mlua::Lua, val: &serde_json::Value) -> mlua::Result<mlua::Value> {
    match val {
        serde_json::Value::Object(map) => {
            let table = lua.create_table()?;
            for (k, v) in map {
                table.set(k.as_str(), json_to_lua(lua, v)?)?;
            }
            Ok(mlua::Value::Table(table))
        }
        serde_json::Value::Array(arr) => {
            let table = lua.create_table()?;
            for (i, v) in arr.iter().enumerate() {
                table.set(i + 1, json_to_lua(lua, v)?)?;
            }
            Ok(mlua::Value::Table(table))
        }
        serde_json::Value::String(s) => {
            Ok(mlua::Value::String(lua.create_string(s)?))
        }
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(mlua::Value::Integer(i))
            } else {
                Ok(mlua::Value::Number(n.as_f64().unwrap_or(0.0)))
            }
        }
        serde_json::Value::Bool(b) => Ok(mlua::Value::Boolean(*b)),
        serde_json::Value::Null => Ok(mlua::Value::Nil),
    }
}

fn lua_value_to_json_string(val: mlua::Value) -> Result<Option<String>> {
    match val {
        mlua::Value::Nil => Ok(None),
        mlua::Value::Table(t) => {
            let mut map = serde_json::Map::new();
            for pair in t.pairs::<mlua::Value, mlua::Value>() {
                let (k, v) = pair?;
                let key = match k {
                    mlua::Value::String(s) => s.to_str()?.to_owned(),
                    mlua::Value::Integer(i) => i.to_string(),
                    _ => continue,
                };
                let json_val = lua_val_to_json(v)?;
                map.insert(key, json_val);
            }
            if map.is_empty() {
                Ok(None)
            } else {
                Ok(Some(serde_json::to_string(&serde_json::Value::Object(map))?))
            }
        }
        _ => Ok(None),
    }
}

fn lua_val_to_json(val: mlua::Value) -> Result<serde_json::Value> {
    match val {
        mlua::Value::Nil => Ok(serde_json::Value::Null),
        mlua::Value::Boolean(b) => Ok(serde_json::Value::Bool(b)),
        mlua::Value::Integer(i) => Ok(serde_json::Value::Number(i.into())),
        mlua::Value::Number(f) => Ok(serde_json::Value::Number(
            serde_json::Number::from_f64(f).unwrap_or(serde_json::Number::from(0)),
        )),
        mlua::Value::String(s) => Ok(serde_json::Value::String(s.to_str()?.to_owned())),
        mlua::Value::Table(t) => {
            let mut map = serde_json::Map::new();
            for pair in t.pairs::<mlua::Value, mlua::Value>() {
                let (k, v) = pair?;
                let key = match k {
                    mlua::Value::String(s) => s.to_str()?.to_owned(),
                    mlua::Value::Integer(i) => i.to_string(),
                    _ => continue,
                };
                map.insert(key, lua_val_to_json(v)?);
            }
            Ok(serde_json::Value::Object(map))
        }
        _ => Ok(serde_json::Value::Null),
    }
}

// ---------------------------------------------------------------------------
// decode_event_return
// ---------------------------------------------------------------------------

fn decode_event_return(json_opt: Option<String>) -> Result<Option<EventReturn>> {
    let Some(s) = json_opt else {
        return Ok(None);
    };
    let v: serde_json::Value = serde_json::from_str(&s)?;
    let obj = v
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("event return: not an object"))?;

    if obj.contains_key("system_prompt_append") || obj.contains_key("system_prompt_prepend") {
        return Ok(Some(EventReturn::SystemPromptMod {
            append: obj
                .get("system_prompt_append")
                .and_then(|v| v.as_str())
                .map(str::to_owned),
            prepend: obj
                .get("system_prompt_prepend")
                .and_then(|v| v.as_str())
                .map(str::to_owned),
        }));
    }

    if obj.get("block").and_then(|v| v.as_bool()).unwrap_or(false) {
        let reason = obj
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("blocked")
            .to_owned();
        return Ok(Some(EventReturn::ToolBlock { reason }));
    }

    if obj.contains_key("result") {
        let content = obj
            .get("result")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        return Ok(Some(EventReturn::ToolResultOverride { content }));
    }

    if obj.contains_key("action") {
        let action = match obj.get("action").and_then(|v| v.as_str()) {
            Some("transform") => InputActionKind::Transform,
            Some("handled") => InputActionKind::Handled,
            _ => InputActionKind::Continue,
        };
        let text = obj
            .get("text")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        return Ok(Some(EventReturn::InputAction { action, text }));
    }

    if let Some(prepend_arr) = obj.get("prepend_messages").and_then(|v| v.as_array()) {
        let prepend = prepend_arr
            .iter()
            .filter_map(|item| {
                let role = item.get("role")?.as_str()?.to_owned();
                let content = item.get("content")?.as_str()?.to_owned();
                Some((role, content))
            })
            .collect();
        return Ok(Some(EventReturn::MessageInjection { prepend }));
    }

    Ok(None)
}

// ---------------------------------------------------------------------------
// parse_event_kind
// ---------------------------------------------------------------------------

fn parse_event_kind(s: &str) -> Result<EventKind> {
    match s {
        "runtime_start" => Ok(EventKind::RuntimeStart),
        "session_start" => Ok(EventKind::SessionStart),
        "tui_ready" => Ok(EventKind::TuiReady),
        "agent_start" => Ok(EventKind::AgentStart),
        "agent_end" => Ok(EventKind::AgentEnd),
        "turn_start" => Ok(EventKind::TurnStart),
        "turn_end" => Ok(EventKind::TurnEnd),
        "before_agent_start" => Ok(EventKind::BeforeAgentStart),
        "context" => Ok(EventKind::Context),
        "tool_call" => Ok(EventKind::ToolCall),
        "tool_result" => Ok(EventKind::ToolResult),
        "input" => Ok(EventKind::Input),
        "session_shutdown" => Ok(EventKind::SessionShutdown),
        other => Err(anyhow::anyhow!("unknown event kind: {other}")),
    }
}
