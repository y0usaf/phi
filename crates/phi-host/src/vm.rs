//! The Lua VM thread and the coroutine dispatch seam.
//!
//! The VM lives on one dedicated OS thread with a current-thread tokio
//! runtime; the host talks to it over a channel. Every dispatch into Lua
//! runs the target function as a Lua coroutine driven as a future, so
//! handlers may await host futures (`pi.sleep`, later: provider streams,
//! subprocesses) — the async seam locked in DESIGN.md.
//!
//! Watchdog semantics (doctrine 02, adapted to the seam): the budget
//! counts *Lua execution slices only*. Each poll of the coroutine future
//! is a slice; time suspended awaiting a host future is free. A pure-Lua
//! busy loop is killed by the instruction hook; awaits are unbounded by
//! design (provider streams run for minutes). Host bindings are async
//! through tokio, so blocking C calls do not exist on this surface.

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{Receiver, Sender, SyncSender, sync_channel};
use std::time::Instant;

use crate::api;
use crate::convert::{json_to_lua, lua_to_json};
use crate::error::{HostError, WATCHDOG_MARKER};
use crate::{HostConfig, Outcome};

/// Check the watchdog deadline every N VM instructions.
const NTH_INSTRUCTION: u32 = 1000;

pub(crate) enum Msg {
    /// Execute an extension chunk (registrations happen as side effects).
    /// The chunk runs on the coroutine path too, so top-level awaits work.
    Load {
        /// Chunk name for error messages and source attribution.
        source_key: String,
        source: String,
        reply: SyncSender<Result<(), HostError>>,
    },
    /// Call every handler subscribed to `event`, sequentially in
    /// registration order, each under its own watchdog window; one hung or
    /// failing handler doesn't stop the rest.
    Emit {
        event: String,
        payload: serde_json::Value,
        reply: SyncSender<Vec<Outcome>>,
    },
}

/// Spawn the dedicated VM thread. Returns after the VM has initialized
/// (or failed to).
pub(crate) fn spawn(config: HostConfig) -> Result<Sender<Msg>, HostError> {
    let (tx, rx) = std::sync::mpsc::channel::<Msg>();
    let (init_tx, init_rx) = sync_channel::<Result<(), String>>(1);
    std::thread::Builder::new()
        .name("phi-host-lua".to_owned())
        .spawn(move || vm_main(config, rx, init_tx))
        .map_err(|_| HostError::VmUnavailable)?;
    match init_rx.recv() {
        Ok(Ok(())) => Ok(tx),
        Ok(Err(msg)) => Err(HostError::Lua(msg)),
        Err(_) => Err(HostError::VmUnavailable),
    }
}

fn vm_main(config: HostConfig, rx: Receiver<Msg>, init_tx: SyncSender<Result<(), String>>) {
    let init = || -> Result<(mlua::Lua, mlua::Table, tokio::runtime::Runtime), String> {
        let lua = mlua::Lua::new();
        let pi = api::build(&lua).map_err(|e| e.to_string())?;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .map_err(|e| e.to_string())?;
        Ok((lua, pi, rt))
    };
    let (lua, pi, rt) = match init() {
        Ok(parts) => {
            let _ = init_tx.send(Ok(()));
            parts
        }
        Err(msg) => {
            let _ = init_tx.send(Err(msg));
            return;
        }
    };

    while let Ok(msg) = rx.recv() {
        match msg {
            Msg::Load {
                source_key,
                source,
                reply,
            } => {
                let res = load_chunk(&lua, &rt, &config, &pi, &source_key, &source);
                let _ = reply.send(res);
            }
            Msg::Emit {
                event,
                payload,
                reply,
            } => {
                let outcomes = emit(&lua, &rt, &config, &event, &payload);
                let _ = reply.send(outcomes);
            }
        }
    }
}

fn load_chunk(
    lua: &mlua::Lua,
    rt: &tokio::runtime::Runtime,
    config: &HostConfig,
    pi: &mlua::Table,
    source_key: &str,
    source: &str,
) -> Result<(), HostError> {
    let func = lua
        .load(source)
        .set_name(format!("@{source_key}"))
        .into_function()
        .map_err(|e| HostError::Lua(e.to_string()))?;
    api::set_current_source(lua, source_key);
    let res = dispatch(lua, rt, config, func, mlua::Value::Table(pi.clone()));
    api::set_current_source(lua, "<host>");
    res.map(|_| ())
}

fn emit(
    lua: &mlua::Lua,
    rt: &tokio::runtime::Runtime,
    config: &HostConfig,
    event: &str,
    payload: &serde_json::Value,
) -> Vec<Outcome> {
    let handlers = match api::event_handlers(lua, event) {
        Ok(handlers) => handlers,
        Err(e) => {
            return vec![Outcome {
                source: "<host>".to_owned(),
                result: Err(e.to_string()),
            }];
        }
    };

    let mut outcomes = Vec::with_capacity(handlers.len());
    for (source, handler) in handlers {
        // Registrations made from inside a handler attribute to its extension.
        api::set_current_source(lua, &source);
        let result = json_to_lua(lua, payload)
            .map_err(|e| HostError::Lua(e.to_string()))
            .and_then(|arg| dispatch(lua, rt, config, handler, arg))
            .and_then(value_to_reply)
            .map_err(|e| e.to_string());
        outcomes.push(Outcome { source, result });
    }
    api::set_current_source(lua, "<host>");
    outcomes
}

/// Run one Lua function as a coroutine driven to completion on the VM
/// thread's runtime, bounded by the slice-counting watchdog.
fn dispatch(
    lua: &mlua::Lua,
    rt: &tokio::runtime::Runtime,
    config: &HostConfig,
    func: mlua::Function,
    arg: mlua::Value,
) -> Result<mlua::Value, HostError> {
    let budget_ms = config.dispatch_timeout_ms;
    let state = Arc::new(WatchdogState::new(budget_ms));

    let triggers = mlua::HookTriggers::new().every_nth_instruction(NTH_INSTRUCTION);
    let hook_state = Arc::clone(&state);
    lua.set_global_hook(triggers, move |_lua, _debug| {
        if hook_state.exceeded() {
            return Err(mlua::Error::runtime(format!(
                "{WATCHDOG_MARKER} handler exceeded {budget_ms}ms of Lua execution"
            )));
        }
        Ok(mlua::VmState::Continue)
    })
    .map_err(|e| HostError::Lua(e.to_string()))?;

    let res = (|| -> Result<mlua::Value, HostError> {
        let thread = lua
            .create_thread(func)
            .map_err(|e| HostError::Lua(e.to_string()))?;
        let fut = thread
            .into_async::<mlua::Value>(arg)
            .map_err(|e| HostError::Lua(e.to_string()))?;
        rt.block_on(Watched {
            inner: Box::pin(fut),
            state: Arc::clone(&state),
        })
        .map_err(|e| HostError::from_lua_message(e.to_string(), budget_ms))
    })();

    lua.remove_global_hook();
    res
}

/// Watchdog bookkeeping shared between the poll wrapper and the
/// instruction hook. Times are µs since `epoch`; `slice_start_us < 0`
/// means the coroutine is suspended (awaiting) and the clock is not
/// running against the budget.
struct WatchdogState {
    epoch: Instant,
    budget_us: i64,
    consumed_us: AtomicI64,
    slice_start_us: AtomicI64,
}

impl WatchdogState {
    fn new(budget_ms: i64) -> Self {
        Self {
            epoch: Instant::now(),
            budget_us: budget_ms.saturating_mul(1000),
            consumed_us: AtomicI64::new(0),
            slice_start_us: AtomicI64::new(-1),
        }
    }

    fn now_us(&self) -> i64 {
        i64::try_from(self.epoch.elapsed().as_micros()).unwrap_or(i64::MAX)
    }

    fn begin_slice(&self) {
        self.slice_start_us.store(self.now_us(), Ordering::Relaxed);
    }

    fn end_slice(&self) {
        let start = self.slice_start_us.swap(-1, Ordering::Relaxed);
        if start >= 0 {
            self.consumed_us
                .fetch_add(self.now_us().saturating_sub(start), Ordering::Relaxed);
        }
    }

    fn exceeded(&self) -> bool {
        let consumed = self.consumed_us.load(Ordering::Relaxed);
        let start = self.slice_start_us.load(Ordering::Relaxed);
        let running = if start >= 0 {
            self.now_us().saturating_sub(start)
        } else {
            0
        };
        consumed.saturating_add(running) >= self.budget_us
    }
}

/// Future wrapper that meters Lua execution: every poll of the coroutine
/// is a watchdog slice; the time between polls (host future pending) is
/// free.
struct Watched<F> {
    inner: Pin<Box<F>>,
    state: Arc<WatchdogState>,
}

impl<F: Future> Future for Watched<F> {
    type Output = F::Output;

    fn poll(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        self.state.begin_slice();
        let res = self.inner.as_mut().poll(cx);
        self.state.end_slice();
        res
    }
}

/// Serialize a handler return value for the reply. `nil` → `None`; a bare
/// string is shorthand for `{ message = ... }`; anything else is converted
/// as-is and handed to the caller uninterpreted — the host never matches
/// on result variants (DESIGN.md: no closed enums at the seam).
fn value_to_reply(val: mlua::Value) -> Result<Option<serde_json::Value>, HostError> {
    match val {
        mlua::Value::Nil => Ok(None),
        mlua::Value::String(s) => {
            let msg = s.to_str().map_err(|e| HostError::Lua(e.to_string()))?;
            Ok(Some(serde_json::json!({ "message": msg.to_owned() })))
        }
        other => {
            let json = lua_to_json(other).map_err(|e| HostError::Lua(e.to_string()))?;
            Ok(Some(json))
        }
    }
}
