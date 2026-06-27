//! Shared per-run state for subcall handlers.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio_util::sync::CancellationToken;

/// Shared per-run state for subcall handlers. The handler outlives a single
/// run in sessions (the env persists across `send()` calls), so the current
/// cancel token and run start time live behind a mutex refreshed by `begin`.
#[derive(Clone)]
pub struct RunState(Arc<Mutex<(CancellationToken, Instant)>>);

impl Default for RunState {
    fn default() -> Self {
        Self::new()
    }
}

impl RunState {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new((
            CancellationToken::new(),
            Instant::now(),
        ))))
    }

    pub fn begin(&self, cancel: CancellationToken) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = (cancel, Instant::now());
    }

    pub fn cancel(&self) -> CancellationToken {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).0.clone()
    }

    pub fn started(&self) -> Instant {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).1
    }
}
