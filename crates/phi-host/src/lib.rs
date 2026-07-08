//! phi-host — the Lua extension host (DESIGN.md WS1; spec:
//! `ref/pi/packages/coding-agent/src/core/extensions/`).
//!
//! The substrate half of the extension seam: an mlua VM on a dedicated
//! thread, an event bus with an open string vocabulary, the coroutine
//! async seam (handlers may await host futures — the locked decision), and
//! a per-dispatch watchdog that meters Lua execution slices only.
//!
//! The bridge is the canonical API (Lua-first): extension chunks are
//! called with the `pi` API table as their single argument — the Lua
//! mirror of the spec's `export default function (pi)`. Extensions receive
//! plain tables and return plain tables; results come back to the caller
//! as uninterpreted JSON values — the host never matches on result
//! variants. This crate is headless by design: no terminal, no rendering,
//! no prompting.

use std::sync::mpsc::{Sender, sync_channel};

mod api;
mod convert;
mod error;
mod vm;

pub use error::HostError;

/// Default per-dispatch watchdog budget in milliseconds of *Lua execution*
/// (time suspended awaiting host futures is free).
pub const DEFAULT_DISPATCH_TIMEOUT_MS: i64 = 5000;

#[derive(Debug, Clone)]
pub struct HostConfig {
    /// Watchdog budget per host→Lua dispatch, in milliseconds of Lua
    /// execution time.
    pub dispatch_timeout_ms: i64,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            dispatch_timeout_ms: DEFAULT_DISPATCH_TIMEOUT_MS,
        }
    }
}

/// Outcome of one handler dispatch, attributed to the extension that
/// registered it. The payload is the handler's return value (`None` when
/// the handler returned nothing); errors are strings so one failing
/// handler never poisons the batch.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub source: String,
    pub result: Result<Option<serde_json::Value>, String>,
}

/// Handle to the Lua VM thread. Cheap to clone; all methods are
/// synchronous from the caller's side (dispatch runs on the VM thread).
#[derive(Clone)]
pub struct Host {
    tx: Sender<vm::Msg>,
}

impl Host {
    /// Start the VM thread and install the `pi` API.
    pub fn new(config: HostConfig) -> Result<Self, HostError> {
        let tx = vm::spawn(config)?;
        Ok(Self { tx })
    }

    /// Load an extension chunk. `source_key` attributes registrations and
    /// error messages (a path for on-disk extensions, a symbolic key for
    /// embedded packs). The chunk runs on the coroutine path, so top-level
    /// awaits work.
    pub fn load(&self, source_key: &str, source: &str) -> Result<(), HostError> {
        let (reply, rx) = sync_channel(1);
        self.tx
            .send(vm::Msg::Load {
                source_key: source_key.to_owned(),
                source: source.to_owned(),
                reply,
            })
            .map_err(|_| HostError::VmUnavailable)?;
        rx.recv().map_err(|_| HostError::VmUnavailable)?
    }

    /// Emit an event to every subscribed handler, sequentially in
    /// registration order (spec: `runner.ts` emit semantics). One failing
    /// or hung handler doesn't stop the rest; each outcome is attributed.
    pub fn emit(
        &self,
        event: &str,
        payload: &serde_json::Value,
    ) -> Result<Vec<Outcome>, HostError> {
        let (reply, rx) = sync_channel(1);
        self.tx
            .send(vm::Msg::Emit {
                event: event.to_owned(),
                payload: payload.clone(),
                reply,
            })
            .map_err(|_| HostError::VmUnavailable)?;
        rx.recv().map_err(|_| HostError::VmUnavailable)
    }
}
