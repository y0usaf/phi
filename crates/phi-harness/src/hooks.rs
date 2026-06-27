//! Typed hooks: observe / on / emit with sequential result chaining.
//!
//! Additive to the existing untyped event system in `phi-ext` /
//! `phi-runtime`; the harness owns a [`PhiHooks`] instance and frontends
//! register typed handlers via [`PhiHooks::on`].

use std::any::{Any, TypeId};
use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;
use std::sync::Mutex as StdMutex;
use tokio::sync::{Mutex, RwLock};

use crate::HarnessError;

/// A typed hook event. Each event defines its own `Result` type so the
/// `emit` reducer can chain handler outputs.
pub trait HookEvent: Send + Sync + 'static {
    type Result: Send + Sync + Default;
}

/// A handler that participates in event semantics and may return a result.
pub type HookHandler<E> =
    Arc<dyn Fn(&E) -> Result<Option<<E as HookEvent>::Result>, HarnessError> + Send + Sync>;

/// A read-only observer that sees all events. Return ignored.
pub type HookObserver = Arc<dyn Fn(&dyn Any) -> Result<(), HarnessError> + Send + Sync>;

/// Provenance metadata for a registered handler / observer.
#[derive(Clone, Debug, Default)]
pub struct SourceInfo {
    pub extension_id: String,
    pub label: Option<String>,
}

// ── Concrete event types ────────────────────────────────────────────────────

/// Context transform: each handler sees current messages.
pub struct ContextEvent {
    pub messages: Vec<phi_rlm::ChatMessage>,
}
impl HookEvent for ContextEvent {
    type Result = ContextResult;
}
#[derive(Default)]
pub struct ContextResult {
    pub messages: Option<Vec<phi_rlm::ChatMessage>>,
}

/// Tool call: early-exit on block.
pub struct ToolCallEvent {
    pub tool_name: String,
    pub input: Value,
}
impl HookEvent for ToolCallEvent {
    type Result = ToolCallResult;
}
#[derive(Default)]
pub struct ToolCallResult {
    pub block: bool,
    pub reason: Option<String>,
}

/// Tool result: patch accumulation.
pub struct ToolResultEvent {
    pub tool_name: String,
    pub content: String,
    pub is_error: bool,
}
impl HookEvent for ToolResultEvent {
    type Result = ToolResultPatch;
}
#[derive(Default)]
pub struct ToolResultPatch {
    pub content: Option<String>,
    pub is_error: Option<bool>,
}

/// Before agent start: collect injected messages, chain system prompt.
pub struct BeforeAgentStartEvent {
    pub system_prompt: String,
    pub prompt: String,
}
impl HookEvent for BeforeAgentStartEvent {
    type Result = BeforeAgentStartResult;
}
#[derive(Default)]
pub struct BeforeAgentStartResult {
    pub messages: Vec<phi_rlm::ChatMessage>,
    pub system_prompt: Option<String>,
}

/// Observation-only: run end.
pub struct RunEndEvent {
    pub stop_cause: phi_rlm::StopCause,
}
impl HookEvent for RunEndEvent {
    type Result = ();
}

/// Observation-only: run start.
pub struct RunStartEvent {
    pub session_id: String,
}
impl HookEvent for RunStartEvent {
    type Result = ();
}

// ── PhiHooks ────────────────────────────────────────────────────────────────

struct HandlerEntry {
    source: SourceInfo,
    handler: Arc<dyn Any + Send + Sync>,
}

impl Clone for HandlerEntry {
    fn clone(&self) -> Self {
        Self {
            source: self.source.clone(),
            handler: self.handler.clone(),
        }
    }
}

struct CleanupEntry {
    cleanup: Box<dyn FnOnce() -> Result<(), HarnessError> + Send + Sync>,
}

/// The hook registry.
pub struct PhiHooks {
    observers: Arc<Mutex<Vec<(SourceInfo, HookObserver)>>>,
    handlers: Arc<RwLock<BTreeMap<TypeId, Vec<HandlerEntry>>>>,
    cleanups: StdMutex<Vec<CleanupEntry>>,
}

impl PhiHooks {
    pub fn new() -> Self {
        Self {
            observers: Arc::new(Mutex::new(Vec::new())),
            handlers: Arc::new(RwLock::new(BTreeMap::new())),
            cleanups: StdMutex::new(Vec::new()),
        }
    }

    /// Register a read-only observer. Returns a cleanup closure that
    /// replaces the observer with a no-op.
    pub async fn observe(&self, observer: HookObserver) -> impl FnOnce() {
        let mut guard = self.observers.lock().await;
        let idx = guard.len();
        guard.push((SourceInfo::default(), observer));
        // Hold a Weak reference so the closure can detect if `PhiHooks` has
        // been dropped before attempting to access the observers list.
        let observers = Arc::downgrade(&self.observers);
        drop(guard);
        move || {
            if let Some(observers) = observers.upgrade()
                && let Ok(mut g) = observers.try_lock()
                && let Some(entry) = g.get_mut(idx) {
                entry.1 = Arc::new(|_| Ok(()));
            }
            // If upgrade returns None, the PhiHooks has been dropped already;
            // nothing to clean up.
        }
    }

    /// Register a typed handler for event `E`. Returns a cleanup closure.
    pub async fn on<E: HookEvent>(
        &self,
        source: SourceInfo,
        handler: HookHandler<E>,
    ) -> impl FnOnce() {
        let mut guard = self.handlers.write().await;
        let type_id = TypeId::of::<E>();
        let vec = guard.entry(type_id).or_default();
        let idx = vec.len();
        vec.push(HandlerEntry {
            source,
            handler: Arc::new(handler),
        });
        // Hold a weak reference similar to observe.
        let handlers = Arc::downgrade(&self.handlers);
        drop(guard);
        move || {
            if let Some(handlers) = handlers.upgrade()
                && let Ok(mut g) = handlers.try_write()
                && let Some(vec) = g.get_mut(&type_id)
                && let Some(entry) = vec.get_mut(idx) {
                let noop: HookHandler<E> = Arc::new(|_: &E| Ok(None));
                entry.handler = Arc::new(noop) as Arc<dyn Any + Send + Sync>;
            }
            // If upgrade returns None, the PhiHooks has been dropped already;
            // nothing to clean up.
        }
    }

    /// Emit a `ContextEvent` (sequential reducer: each handler sees current
    /// messages; the last non-None modification wins).
    pub async fn emit_context(
        &self,
        mut event: ContextEvent,
    ) -> Result<Option<ContextResult>, HarnessError> {
        let entries = self.snapshot_handlers::<ContextEvent>().await;
        let mut modified = false;
        for entry in &entries {
            let handler = entry
                .handler
                .downcast_ref::<HookHandler<ContextEvent>>()
                .unwrap();
            if let Some(r) = handler(&event)?
                && let Some(msgs) = r.messages {
                    event.messages = msgs;
                    modified = true;
                }
        }
        Ok(if modified {
            Some(ContextResult {
                messages: Some(event.messages),
            })
        } else {
            None
        })
    }

    /// Emit a `ToolCallEvent` (early-exit on block).
    pub async fn emit_tool_call(
        &self,
        event: ToolCallEvent,
    ) -> Result<Option<ToolCallResult>, HarnessError> {
        let entries = self.snapshot_handlers::<ToolCallEvent>().await;
        for entry in &entries {
            let handler = entry
                .handler
                .downcast_ref::<HookHandler<ToolCallEvent>>()
                .unwrap();
            if let Some(r) = handler(&event)?
                && r.block {
                    return Ok(Some(r));
                }
        }
        Ok(None)
    }

    /// Emit a `ToolResultEvent` (patch accumulation).
    pub async fn emit_tool_result(
        &self,
        mut event: ToolResultEvent,
    ) -> Result<Option<ToolResultPatch>, HarnessError> {
        let entries = self.snapshot_handlers::<ToolResultEvent>().await;
        let mut modified = false;
        for entry in &entries {
            let handler = entry
                .handler
                .downcast_ref::<HookHandler<ToolResultEvent>>()
                .unwrap();
            if let Some(patch) = handler(&event)? {
                if let Some(content) = patch.content {
                    event.content = content;
                    modified = true;
                }
                if let Some(is_err) = patch.is_error {
                    event.is_error = is_err;
                    modified = true;
                }
            }
        }
        Ok(if modified {
            Some(ToolResultPatch {
                content: Some(event.content),
                is_error: Some(event.is_error),
            })
        } else {
            None
        })
    }

    /// Emit a `BeforeAgentStartEvent` (collect messages, chain system prompt).
    pub async fn emit_before_agent_start(
        &self,
        event: BeforeAgentStartEvent,
    ) -> Result<BeforeAgentStartResult, HarnessError> {
        let entries = self.snapshot_handlers::<BeforeAgentStartEvent>().await;
        let mut messages = Vec::new();
        let mut system_prompt = event.system_prompt.clone();
        let prompt = event.prompt.clone();
        let mut modified_prompt = false;
        for entry in &entries {
            let handler = entry
                .handler
                .downcast_ref::<HookHandler<BeforeAgentStartEvent>>()
                .unwrap();
            // Each handler sees the original event; accumulated messages are
            // collected from every handler's result.
            let ev = BeforeAgentStartEvent {
                system_prompt: system_prompt.clone(),
                prompt: prompt.clone(),
            };
            if let Some(r) = handler(&ev)? {
                messages.extend(r.messages);
                if let Some(sp) = r.system_prompt {
                    system_prompt = sp;
                    modified_prompt = true;
                }
            }
        }
        Ok(BeforeAgentStartResult {
            messages,
            system_prompt: if modified_prompt {
                Some(system_prompt)
            } else {
                None
            },
        })
    }

    /// Emit an observation-only event (run start/end). Result is ().
    pub async fn emit_observation<E: HookEvent<Result = ()>>(
        &self,
        event: E,
    ) -> Result<(), HarnessError> {
        self.run_observers(&event).await?;
        let entries = self.snapshot_handlers::<E>().await;
        for entry in &entries {
            let handler = entry.handler.downcast_ref::<HookHandler<E>>().unwrap();
            handler(&event)?;
        }
        Ok(())
    }

    async fn run_observers<E: Any + Send + Sync>(&self, event: &E) -> Result<(), HarnessError> {
        let observers = self.observers.lock().await;
        for (_, observer) in observers.iter() {
            let _ = observer(event as &dyn Any);
        }
        Ok(())
    }

    async fn snapshot_handlers<E: HookEvent>(&self) -> Vec<HandlerEntry> {
        let guard = self.handlers.read().await;
        guard.get(&TypeId::of::<E>()).cloned().unwrap_or_default()
    }

    /// Register a cleanup function.
    pub fn add_cleanup<F>(&self, _source: SourceInfo, cleanup: F)
    where
        F: FnOnce() -> Result<(), HarnessError> + Send + Sync + 'static,
    {
        self.cleanups.lock().unwrap().push(CleanupEntry {
            cleanup: Box::new(cleanup),
        });
    }

    /// Create a scoped source for registering hooks from an extension.
    pub fn create_scope(&self, source: SourceInfo) -> HookScope<'_> {
        HookScope {
            hooks: self,
            source,
        }
    }

    /// Remove all handlers and run cleanups.
    pub async fn clear(&self) -> Result<(), HarnessError> {
        self.handlers.write().await.clear();
        let mut cleanups = self.cleanups.lock().unwrap();
        for entry in cleanups.drain(..) {
            (entry.cleanup)()?;
        }
        Ok(())
    }
}

impl Default for PhiHooks {
    fn default() -> Self {
        Self::new()
    }
}

/// A scoped source for registering hooks from an extension.
pub struct HookScope<'a> {
    hooks: &'a PhiHooks,
    source: SourceInfo,
}

impl<'a> HookScope<'a> {
    /// Borrow the underlying hooks.
    pub fn hooks(&self) -> &PhiHooks {
        self.hooks
    }

    /// Borrow the source info.
    pub fn source(&self) -> &SourceInfo {
        &self.source
    }
}
