//! Observability: trace/span tree, stable event names, safe payloads.
//!
//! Runtime-agnostic. The harness threads an [`ObservabilityContext`] via
//! `tokio::task_local!` for deterministic span propagation; subscribers
//! receive [`ObservabilityEvent`]s via a process-global static list.

use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex, OnceLock};

use serde::Serialize;
use serde_json::Value;

use crate::HarnessError;

tokio::task_local! {
    static OBS_CONTEXT: ObservabilityContext;
}

/// Per-span context propagated through the trace tree.
#[derive(Clone, Debug, Default)]
pub struct ObservabilityContext {
    pub trace_id: Option<String>,
    pub current_span_id: Option<String>,
    pub user_context: Value,
}

/// One observability event (start/end/error/event) in the trace tree.
#[derive(Clone, Debug, Serialize)]
pub struct ObservabilityEvent {
    pub r#type: ObservabilityEventType,
    pub name: String,
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub timestamp: String,
    pub duration_ms: Option<f64>,
    pub context: Value,
    pub payload: Value,
    pub error: Option<ErrorInfo>,
}

/// Event type discriminator.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservabilityEventType {
    Start,
    End,
    Error,
    Event,
}

/// Error metadata attached to an `Error` event.
#[derive(Clone, Debug, Serialize)]
pub struct ErrorInfo {
    pub name: String,
    pub message: String,
}

/// A subscriber callback invoked for every observability event.
pub type ObservabilitySubscriber = Arc<dyn Fn(&ObservabilityEvent) + Send + Sync>;

static SUBSCRIBERS: OnceLock<Mutex<Vec<ObservabilitySubscriber>>> = OnceLock::new();

/// Register a subscriber. Returns a cleanup closure (best-effort: the
/// subscriber is replaced with a no-op on cleanup, since the static list
/// itself is not mutable).
pub fn subscribe_observability(subscriber: ObservabilitySubscriber) -> impl FnOnce() {
    let subs = SUBSCRIBERS.get_or_init(|| Mutex::new(Vec::new()));
    let mut guard = subs.lock().unwrap_or_else(|e| e.into_inner());
    let idx = guard.len();
    guard.push(subscriber);
    drop(guard);
    // Cleanup: replace the subscriber at `idx` with a no-op. We can't
    // remove from the Vec (indices would shift), so we swap-in a no-op.
    move || {
        if let Some(subs) = SUBSCRIBERS.get() {
            let mut guard = subs.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(slot) = guard.get_mut(idx) {
                *slot = Arc::new(|_| {});
            }
        }
    }
}

/// Emit an observability event to all subscribers. Subscriber errors/panics
/// are swallowed and never affect execution.
async fn emit_observability(event: ObservabilityEvent) {
    if let Some(subs) = SUBSCRIBERS.get() {
        // Poison-safe: if a previous lock holder panicked, recover the
        // inner Vec rather than propagating the poison — subscribers must
        // never affect execution.
        let guard = subs.lock().unwrap_or_else(|e| e.into_inner());
        for sub in guard.iter() {
            let _ = std::panic::catch_unwind(AssertUnwindSafe(|| sub(&event)));
        }
    }
}

/// Trace an operation: emit a `Start` event, run `f`, then emit `End` or
/// `Error`. The parent context is read from the task-local; a child context
/// is scoped to `f`.
pub async fn trace_operation<T, F, Fut>(name: &str, payload: Value, f: F) -> Result<T, HarnessError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T, HarnessError>>,
{
    let parent = OBS_CONTEXT.try_get().unwrap_or_default();
    let trace_id = parent
        .trace_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
    let span_id = uuid::Uuid::now_v7().to_string();
    let parent_span_id = parent.current_span_id.clone();

    let child = ObservabilityContext {
        trace_id: Some(trace_id.clone()),
        current_span_id: Some(span_id.clone()),
        user_context: parent.user_context.clone(),
    };

    emit_observability(ObservabilityEvent {
        r#type: ObservabilityEventType::Start,
        name: name.to_string(),
        trace_id: trace_id.clone(),
        span_id: span_id.clone(),
        parent_span_id: parent_span_id.clone(),
        timestamp: jiff::Timestamp::now().to_string(),
        duration_ms: None,
        context: parent.user_context.clone(),
        payload,
        error: None,
    })
    .await;

    let result = OBS_CONTEXT.scope(child, f()).await;

    match &result {
        Ok(_) => {
            emit_observability(ObservabilityEvent {
                r#type: ObservabilityEventType::End,
                name: name.to_string(),
                trace_id,
                span_id,
                parent_span_id,
                timestamp: jiff::Timestamp::now().to_string(),
                duration_ms: None,
                context: parent.user_context,
                payload: Value::Null,
                error: None,
            })
            .await;
        }
        Err(e) => {
            emit_observability(ObservabilityEvent {
                r#type: ObservabilityEventType::Error,
                name: name.to_string(),
                trace_id,
                span_id,
                parent_span_id,
                timestamp: jiff::Timestamp::now().to_string(),
                duration_ms: None,
                context: parent.user_context,
                payload: Value::Null,
                error: Some(ErrorInfo {
                    name: "HarnessError".to_string(),
                    message: e.to_string(),
                }),
            })
            .await;
        }
    }

    result
}

/// Safe payload redaction: returns only safe fields. Unsafe content
/// (prompts, completions, tool args/results, file contents, api keys) is
/// never included. When unsure, returns `Value::Null`.
pub fn safe_payload(name: &str, payload: &Value) -> Value {
    let safe_keys: &[&str] = &[
        "provider",
        "model",
        "session_id",
        "entry_type",
        "tool_name",
        "status_code",
        "stop_reason",
        "input_tokens",
        "output_tokens",
        "cache_read",
        "cache_write",
        "total_tokens",
        "cost",
        "duration_ms",
        "iterations",
        "depth",
    ];
    if let Some(obj) = payload.as_object() {
        let filtered: serde_json::Map<String, Value> = obj
            .iter()
            .filter(|(k, _)| safe_keys.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        Value::Object(filtered)
    } else {
        // Non-object payloads carry no safe key/value structure to filter, so
        // redact them to null regardless of event name. (Safe by over-redaction:
        // a raw scalar payload could be a prompt or completion, which must never
        // pass through. `name` is retained for future structured allow-listing.)
        let _ = name;
        Value::Null
    }
}
