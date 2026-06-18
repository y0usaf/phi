use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use phi_ext::{
    CommandInvocation, CommandOutput, CommandSpec, EventHandlerRegistration,
    ExtensionHost, ExtensionManifest, LifecycleEvent,
};

mod bridge;
mod convert;
mod event_return;
mod globals;
mod thread;

pub use thread::LuaMsg;

use event_return::{decode_event_return, parse_event_kind};
use globals::setup_phi_global;
use thread::spawn_lua_thread;

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