//! phi-host — the Lua extension host (DESIGN.md WS1).
//!
//! The substrate half of the extension seam: an mlua VM on a dedicated
//! message thread, `register_*` registries, an event bus with an open
//! string vocabulary, a per-dispatch watchdog, extension discovery, and a
//! trust store gating project-local Lua.
//!
//! The bridge is the canonical API (Lua-first): extensions receive plain
//! tables and return plain tables; results come back to the caller as
//! uninterpreted JSON values — the host never matches on result variants.
//! This crate is headless by design (frontend-seam decision): no terminal,
//! no rendering, no prompting.

use std::path::Path;
use std::sync::mpsc::{Sender, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};

mod convert;
mod discovery;
mod globals;
mod thread;
mod trust;

pub use discovery::{
    discover_extensions, global_extensions_dir, project_extensions_dir, project_extensions_present,
};
pub use trust::{TrustDecision, TrustStore};

/// Default per-dispatch watchdog budget (recorded in DESIGN.md).
pub const DEFAULT_DISPATCH_TIMEOUT_MS: i64 = 5000;

/// Margin the host waits beyond the watchdog before declaring the VM
/// thread unresponsive (a blocking C call the instruction hook can't
/// interrupt).
const REPLY_MARGIN: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct HostConfig {
    /// Watchdog budget per host→Lua dispatch, in milliseconds.
    pub dispatch_timeout_ms: i64,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            dispatch_timeout_ms: DEFAULT_DISPATCH_TIMEOUT_MS,
        }
    }
}

/// A command registered through `phi.register_command`.
#[derive(Debug, Clone)]
pub struct CommandInfo {
    pub name: String,
    pub description: Option<String>,
    /// Path (or embedded key) of the registering extension.
    pub source: String,
}

/// A tool registered through `phi.register_tool` (Lua) or
/// [`Host::register_tool`] (native). Same registry either way — the loop
/// pack's builtins go through the door a user extension would use.
#[derive(Debug, Clone)]
pub struct ToolInfo {
    pub name: String,
    /// Sent to the model — what the tool does and when to use it.
    pub description: String,
    /// JSON Schema for the tool's arguments.
    pub input_schema: serde_json::Value,
    /// Path (or embedded key) of the registering extension; `<native>`
    /// prefixed names for Rust-side registrations.
    pub source: String,
}

/// Result of one tool invocation, as the model will see it.
#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub content: String,
    /// Tool-level failure (bad args, exec error, watchdog kill). Relayed to
    /// the model as an error tool-result, not surfaced as a host error.
    pub is_error: bool,
}

/// A native (Rust) tool handler. Runs on the caller's thread — long-running
/// tools (bash) don't occupy the Lua VM thread or its watchdog.
pub type NativeToolFn = Box<dyn Fn(&serde_json::Value) -> Result<String, String> + Send + Sync>;

/// Outcome of one event-handler dispatch. `result` is the handler's return
/// value as a plain JSON value (`None` when it returned nothing), or the
/// error text when it failed or was killed by the watchdog.
#[derive(Debug)]
pub struct Dispatch {
    pub source: String,
    pub result: Result<Option<serde_json::Value>, String>,
}

/// Handle to the extension host. Cheap to share behind a reference; all
/// calls serialize onto the VM thread.
pub struct Host {
    tx: Sender<thread::LuaMsg>,
    meta: Arc<Mutex<globals::Meta>>,
    /// Handlers for natively-registered tools (metadata lives in `meta`
    /// beside the Lua-registered ones).
    native_tools: Mutex<std::collections::HashMap<String, Arc<NativeToolFn>>>,
    timeout_ms: i64,
}

impl Host {
    pub fn new() -> Result<Self> {
        Self::with_config(HostConfig::default())
    }

    pub fn with_config(config: HostConfig) -> Result<Self> {
        let lua = mlua::Lua::new();
        let meta = Arc::new(Mutex::new(globals::Meta::default()));
        globals::install(&lua, Arc::clone(&meta))
            .map_err(|e| anyhow!("installing phi globals: {e}"))?;
        let (tx, rx) = std::sync::mpsc::channel();
        thread::spawn(lua, rx);
        Ok(Self {
            tx,
            meta,
            native_tools: Mutex::new(std::collections::HashMap::new()),
            timeout_ms: config.dispatch_timeout_ms,
        })
    }

    /// Load an extension file. Registration errors and syntax errors are
    /// reported per-file; the VM stays usable either way.
    pub fn load_file(&self, path: &Path) -> Result<()> {
        let source = std::fs::read_to_string(path)
            .with_context(|| format!("reading extension {}", path.display()))?;
        self.load_source(&path.display().to_string(), &source)
    }

    /// Load an extension from a string — the path embedded shipped
    /// defaults take (DESIGN.md locked decision), and the test seam.
    pub fn load_source(&self, source_key: &str, source: &str) -> Result<()> {
        let (reply, rx) = sync_channel(1);
        self.tx
            .send(thread::LuaMsg::Load {
                source_key: source_key.to_owned(),
                source: source.to_owned(),
                timeout_ms: self.timeout_ms,
                reply,
            })
            .map_err(|_| anyhow!("lua thread is gone"))?;
        self.recv(rx)?
            .with_context(|| format!("loading extension {source_key}"))
    }

    /// Registered commands, in registration order.
    pub fn commands(&self) -> Vec<CommandInfo> {
        self.meta.lock().expect("meta lock").commands.clone()
    }

    /// Registered tools, in registration order (native and Lua together).
    pub fn tools(&self) -> Vec<ToolInfo> {
        self.meta.lock().expect("meta lock").tools.clone()
    }

    /// Register a native (Rust) tool into the same registry
    /// `phi.register_tool` writes to. `source` attributes the registration
    /// (e.g. `<native:phi-agent>`); duplicate names fail, naming the prior
    /// source, exactly as a duplicate Lua registration would.
    pub fn register_tool(
        &self,
        name: &str,
        description: &str,
        input_schema: serde_json::Value,
        source: &str,
        handler: NativeToolFn,
    ) -> Result<()> {
        if name.trim().is_empty() {
            bail!("register_tool: 'name' must be a non-empty string");
        }
        let mut meta = self.meta.lock().expect("meta lock");
        if let Some(prior) = meta.tools.iter().find(|t| t.name == name) {
            bail!(
                "register_tool('{name}'): already registered by {}",
                prior.source
            );
        }
        meta.tools.push(ToolInfo {
            name: name.to_owned(),
            description: description.to_owned(),
            input_schema,
            source: source.to_owned(),
        });
        self.native_tools
            .lock()
            .expect("native tools lock")
            .insert(name.to_owned(), Arc::new(handler));
        Ok(())
    }

    /// Invoke a registered tool with JSON arguments. Tool-level failures
    /// (handler error, watchdog kill) come back as an `is_error` output for
    /// the model; only host-level problems (unknown tool, dead VM thread)
    /// are `Err`. Native handlers run on this thread; Lua handlers dispatch
    /// to the VM thread under the watchdog.
    pub fn invoke_tool(&self, name: &str, args: &serde_json::Value) -> Result<ToolOutput> {
        let native = {
            let map = self.native_tools.lock().expect("native tools lock");
            map.get(name).cloned()
        };
        if let Some(handler) = native {
            return Ok(match handler(args) {
                Ok(content) => ToolOutput {
                    content,
                    is_error: false,
                },
                Err(content) => ToolOutput {
                    content,
                    is_error: true,
                },
            });
        }

        if !self.tools().iter().any(|t| t.name == name) {
            bail!("unknown tool '{name}'");
        }
        let (reply, rx) = sync_channel(1);
        self.tx
            .send(thread::LuaMsg::InvokeTool {
                name: name.to_owned(),
                args_json: serde_json::to_string(args)?,
                timeout_ms: self.timeout_ms,
                reply,
            })
            .map_err(|_| anyhow!("lua thread is gone"))?;
        Ok(match self.recv(rx)? {
            Ok(content) => ToolOutput {
                content: content.unwrap_or_default(),
                is_error: false,
            },
            Err(e) => ToolOutput {
                content: format!("{e:#}"),
                is_error: true,
            },
        })
    }

    /// Number of handlers subscribed to an event.
    pub fn subscriber_count(&self, event: &str) -> usize {
        self.meta
            .lock()
            .expect("meta lock")
            .events
            .get(event)
            .copied()
            .unwrap_or(0)
    }

    /// Dispatch an event to every subscribed handler in registration
    /// order. Each handler runs under its own watchdog window; a hung or
    /// failing handler is reported in its [`Dispatch`] and the rest still
    /// run. An event with no subscribers returns an empty vec.
    pub fn emit(&self, event: &str, payload: &serde_json::Value) -> Result<Vec<Dispatch>> {
        let (reply, rx) = sync_channel(1);
        self.tx
            .send(thread::LuaMsg::Emit {
                event: event.to_owned(),
                payload_json: serde_json::to_string(payload)?,
                timeout_ms: self.timeout_ms,
                reply,
            })
            .map_err(|_| anyhow!("lua thread is gone"))?;
        // Reply budget scales with the number of handlers, each of which
        // may consume a full watchdog window.
        let handlers = self.subscriber_count(event).max(1) as u32;
        let deadline =
            Duration::from_millis(self.timeout_ms.max(0) as u64) * handlers + REPLY_MARGIN;
        let outcomes = rx
            .recv_timeout(deadline)
            .map_err(|_| anyhow!("lua thread unresponsive dispatching '{event}'"))?;
        Ok(outcomes
            .into_iter()
            .map(|o| Dispatch {
                source: o.source,
                result: o.result.map(|json| {
                    json.map(|text| serde_json::from_str(&text).unwrap_or(serde_json::Value::Null))
                }),
            })
            .collect())
    }

    /// Invoke a registered command with a raw argument string. Returns the
    /// handler's result as a plain JSON value (a bare-string return is
    /// shorthand for `{ "message": ... }`), `None` when it returned
    /// nothing. Unknown commands are an error.
    pub fn invoke_command(&self, name: &str, args: &str) -> Result<Option<serde_json::Value>> {
        let (reply, rx) = sync_channel(1);
        self.tx
            .send(thread::LuaMsg::InvokeCommand {
                name: name.to_owned(),
                args: args.to_owned(),
                timeout_ms: self.timeout_ms,
                reply,
            })
            .map_err(|_| anyhow!("lua thread is gone"))?;
        let json = self.recv(rx)??;
        match json {
            Some(text) => Ok(Some(serde_json::from_str(&text)?)),
            None => Ok(None),
        }
    }

    fn recv<T>(&self, rx: std::sync::mpsc::Receiver<T>) -> Result<T> {
        let deadline = Duration::from_millis(self.timeout_ms.max(0) as u64) + REPLY_MARGIN;
        match rx.recv_timeout(deadline) {
            Ok(v) => Ok(v),
            Err(_) => bail!("lua thread unresponsive (blocked past the watchdog margin)"),
        }
    }
}
