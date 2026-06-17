# PHASE 4: DURABILITY + OBSERVABILITY — Implementation Plan

## 1. New Durable Entry Types

Phi extends the existing JSONL session schema with operation-level durable entries. Each entry is a single JSON object per line, tagged with a `type` discriminator field for serde internally-tagged representation.

### 1.1 Enum: SessionEntry

All existing and new entry types are unified under one internally-tagged enum:

```rust
#[serde(tag = "type")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SessionEntry {
    // Existing entries
    Session(SessionHeader),
    Message(MessageEntry),
    Repl(ReplEntry),
    ExchangeStart(ExchangeStartEntry),
    ExchangeEnd(ExchangeEndEntry),
    Leaf(LeafEntry),
    // New durable entries
    OperationStart(OperationStartEntry),
    OperationFinish(OperationFinishEntry),
    OperationInterrupted(OperationInterruptedEntry),
    TurnStart(TurnStartEntry),
    TurnFinish(TurnFinishEntry),
    ToolCallStart(ToolCallStartEntry),
    ToolCallFinish(ToolCallFinishEntry),
    QueueEnqueued(QueueEnqueuedEntry),
    QueueConsumed(QueueConsumedEntry),
    PendingWriteEnqueued(PendingWriteEnqueuedEntry),
    PendingWriteApplied(PendingWriteAppliedEntry),
}
```

### 1.2 Operation Entries

Operations are coarse-grained units of work: compaction, tree ops, provider calls, tool calls. Each has a unique `op_id`, a kind, an optional parent for nesting, and a trace/span for observability correlation.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationStartEntry {
    pub op_id: Uuid,
    pub op_kind: OperationKind,
    pub parent_op_id: Option<Uuid>,
    pub trace_id: TraceId,    // newtype wrapper around String
    pub span_id: SpanId,      // newtype wrapper around String
    pub started_at: DateTime<Utc>,
    pub payload: SafePayload, // already-redacted
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationFinishEntry {
    pub op_id: Uuid,
    pub finished_at: DateTime<Utc>,
    pub duration_ms: u64,
    pub result_summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationInterruptedEntry {
    pub op_id: Uuid,
    pub interrupted_at: DateTime<Utc>,
    pub reason: InterruptReason,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    Compaction,
    TreeOp,
    ProviderRequest,
    ToolCall,
    Turn,
    Prompt,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterruptReason {
    Crash,
    Timeout,
    UserAbort,
    DependencyError,
    RecoveryMarked,
}
```
### 1.3 Turn Entries

Turns are the top-level agent loop iteration. `turn_start` records which queue items were consumed, linking durable queue entries to the turn that processed them.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnStartEntry {
    pub turn_id: Uuid,
    pub trace_id: TraceId,
    pub span_id: SpanId,
    pub started_at: DateTime<Utc>,
    pub consumed_queue_ids: Vec<Uuid>,  // links to QueueEnqueued entries
    pub op_id: Uuid,  // parent operation for this turn
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnFinishEntry {
    pub turn_id: Uuid,
    pub finished_at: DateTime<Utc>,
    pub duration_ms: u64,
    pub stop_reason: Option<String>,
}
```

### 1.4 Tool Call Entries

Tool calls are instrumented at the boundary: start records the tool name and a deterministic call ID; finish records duration and a safe status. Tool arguments and results are NOT stored (unsafe).

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallStartEntry {
    pub call_id: Uuid,
    pub tool_name: String,
    pub op_id: Uuid,      // parent operation
    pub turn_id: Uuid,    // parent turn
    pub trace_id: TraceId,
    pub span_id: SpanId,
    pub started_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallFinishEntry {
    pub call_id: Uuid,
    pub finished_at: DateTime<Utc>,
    pub duration_ms: u64,
    pub success: bool,
    pub error_type: Option<String>,  // safe: error category only, not message
}
```

### 1.5 Queue Entries

The durable queue enables crash-safe work item processing. Items are enqueued before work begins and marked consumed when a turn starts processing them.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueEnqueuedEntry {
    pub queue_id: Uuid,
    pub enqueued_at: DateTime<Utc>,
    pub item_kind: QueueItemKind,
    pub item_ref: Option<String>,  // safe reference (e.g. message_id), not content
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueConsumedEntry {
    pub queue_id: Uuid,
    pub consumed_at: DateTime<Utc>,
    pub turn_id: Uuid,  // which turn consumed this
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueItemKind {
    UserMessage,
    CompactionTask,
    TreeOperation,
    SystemTask,
}
```

### 1.6 Pending Write Entries

Pending writes buffer file mutations with deterministic target IDs so they can be applied idempotently during recovery. The target ID is a hash of the target path + content hash, ensuring re-application is safe.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingWriteEnqueuedEntry {
    pub write_id: Uuid,
    pub target_id: String,  // deterministic: blake3(path + content_hash)
    pub target_path: String,
    pub enqueued_at: DateTime<Utc>,
    pub op_id: Uuid,  // parent operation
    pub content_hash: String,  // blake3 hex
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingWriteAppliedEntry {
    pub write_id: Uuid,
    pub applied_at: DateTime<Utc>,
    pub target_id: String,  // matches enqueued target_id
    pub success: bool,
}
```

## 2. Recovery Logic

### 2.1 Recovery State

`PhiHarness::recover()` reduces the JSONL log by folding every entry into a `RecoveryState` struct. This struct accumulates the known state of the session after replay.

```rust
#[derive(Debug, Default)]
pub struct RecoveryState {
    pub session_header: Option<SessionHeader>,
    pub messages: Vec<MessageEntry>,      // DAG nodes
    pub current_leaf: Option<LeafEntry>,
    pub operations: HashMap<Uuid, OperationState>,
    pub turns: HashMap<Uuid, TurnState>,
    pub tool_calls: HashMap<Uuid, ToolCallState>,
    pub queue: HashMap<Uuid, QueueItemState>,
    pub pending_writes: HashMap<Uuid, PendingWriteState>,
}

#[derive(Debug, Default)]
pub struct OperationState {
    pub kind: Option<OperationKind>,
    pub status: OpStatus,
    pub parent_op_id: Option<Uuid>,
    pub trace_id: Option<TraceId>,
    pub span_id: Option<SpanId>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum OpStatus {
    #[default]
    Unknown,    // started entry not found
    Started,
    Finished,
    Interrupted,
}

#[derive(Debug, Default)]
pub struct TurnState {
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub consumed_queue_ids: Vec<Uuid>,
    pub status: OpStatus,
}

#[derive(Debug, Default)]
pub struct ToolCallState {
    pub tool_name: Option<String>,
    pub status: OpStatus,
}

#[derive(Debug, Default)]
pub struct QueueItemState {
    pub kind: Option<QueueItemKind>,
    pub enqueued: bool,
    pub consumed: bool,
    pub consumed_by_turn: Option<Uuid>,
}

#[derive(Debug, Default)]
pub struct PendingWriteState {
    pub target_path: String,
    pub target_id: String,
    pub content_hash: String,
    pub applied: bool,
}
```

### 2.2 Log Reduction Algorithm

Recovery is a left fold over the JSONL log:

```rust
impl PhiHarness {
    pub fn recover(path: &Path) -> Result<RecoveryState, RecoveryError> {
        let reader = BufReader::new(File::open(path)?);
        let mut state = RecoveryState::default();

        for (line_num, line) in reader.lines().enumerate() {
            let line = line.map_err(|e| RecoveryError::IO { line: line_num, source: e })?;
            if line.trim().is_empty() { continue; }
            let entry: SessionEntry = serde_json::from_str(&line)
                .map_err(|e| RecoveryError::Parse { line: line_num, source: e })?;
            Self::apply_entry(&mut state, entry, line_num)?;
        }

        // Post-pass: reconcile unfinished operations
        Self::reconcile(&mut state);
        Ok(state)
    }

    fn apply_entry(state: &mut RecoveryState, entry: SessionEntry, line: usize) -> Result<(), RecoveryError> {
        match entry {
            SessionEntry::Session(h) => state.session_header = Some(h),
            SessionEntry::Message(m) => state.messages.push(m),
            SessionEntry::Leaf(l) => state.current_leaf = Some(l),
            SessionEntry::OperationStart(e) => {
                state.operations.insert(e.op_id, OperationState {
                    kind: Some(e.op_kind),
                    status: OpStatus::Started,
                    parent_op_id: e.parent_op_id,
                    trace_id: Some(e.trace_id),
                    span_id: Some(e.span_id),
                    started_at: Some(e.started_at),
                    finished_at: None,
                });
            }
            SessionEntry::OperationFinish(e) => {
                if let Some(op) = state.operations.get_mut(&e.op_id) {
                    op.status = OpStatus::Finished;
                    op.finished_at = Some(e.finished_at);
                }
            }
            SessionEntry::TurnStart(e) => {
                state.turns.insert(e.turn_id, TurnState {
                    started_at: Some(e.started_at),
                    finished_at: None,
                    consumed_queue_ids: e.consumed_queue_ids.clone(),
                    status: OpStatus::Started,
                });
                // Mark consumed queue items
                for qid in &e.consumed_queue_ids {
                    if let Some(q) = state.queue.get_mut(qid) {
                        q.consumed = true;
                        q.consumed_by_turn = Some(e.turn_id);
                    }
                }
            }
            SessionEntry::TurnFinish(e) => {
                if let Some(t) = state.turns.get_mut(&e.turn_id) {
                    t.status = OpStatus::Finished;
                    t.finished_at = Some(e.finished_at);
                }
            }
            // ... ToolCall, Queue, PendingWrite entries applied similarly
            _ => {} // existing entries (ExchangeStart/End, Repl) handled as before
        }
        Ok(())
    }
}
```

### 2.3 Post-Pass Reconciliation

After the fold, `reconcile()` walks all operations/turns/tool_calls and applies the conservative interruption policy:

```rust
impl PhiHarness {
    fn reconcile(state: &mut RecoveryState) {
        // Any operation still in Started status is unfinished -> mark Interrupted
        for (op_id, op) in &mut state.operations {
            if op.status == OpStatus::Started {
                op.status = OpStatus::Interrupted;
            }
        }
        // Any turn still Started -> mark Interrupted
        for (turn_id, turn) in &mut state.turns {
            if turn.status == OpStatus::Started {
                turn.status = OpStatus::Interrupted;
            }
        }
        // Pending writes: if not applied, re-enqueue for application
        // (deterministic target_id makes re-application idempotent)
        // Tool calls: do NOT retry unless retry-safe. Interrupted tool calls
        // are marked Interrupted and left for the user/agent to decide.
        // Compaction/tree ops: only rerun if no OperationFinish exists.
    }
}
```

**Conservative interruption policy summary:**

| Entry Type | Unfinished State | Action |
|---|---|---|
| Operation | Started (no finish/interrupted) | Mark `Interrupted`. Rerun compaction/tree ops only if no final entry exists. |
| Turn | Started (no turn_finish) | Mark `Interrupted`. Re-enqueue consumed queue items. |
| Tool Call | Started (no tool_call_finish) | Mark `Interrupted`. Do NOT retry unless retry-safe (idempotent tool + no observed side effects). |
| Queue | Enqueued, not consumed | Leave in queue; available for next turn. |
| Pending Write | Enqueued, not applied | Re-enqueue for application. Idempotent via `target_id`. |

### 2.4 Runtime Dependency Validation

During recovery, `PhiHarness` validates that runtime dependencies are available before resuming:

```rust
pub fn validate_runtime_deps(state: &RecoveryState) -> Result<(), RecoveryError> {
    // 1. Check provider API keys are set (env or config)
    // 2. Check working directory exists and is accessible
    // 3. Check session log is writable (for appending recovery markers)
    // 4. Verify any pending_write target directories exist
    Ok(())
}
```

## 3. ObservabilityContext and ObservabilityEvent

### 3.1 Design Decision: tokio::task_local!

Phi uses `tokio::task_local!` for context propagation, not explicit threading. Rationale:

- Phi's engine is fully async under tokio. Every operation runs inside a tokio task.
- Explicit context threading would require adding a `ctx: &ObservabilityContext` parameter to dozens of functions, polluting the call graph and risking dropped context at boundaries.
- `task_local!` provides automatic propagation across `.await` points within the same task, with `Send + Sync` guarantees from tokio's runtime.
- The main drawback — context is lost when spawning a new task — is handled by explicitly passing the context into child spawns (see 3.4).

### 3.2 Struct Definitions

```rust
use std::sync::Arc;
use tokio::task_local;

// Newtype wrappers for type safety
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TraceId(pub String);  // e.g. "trace-<uuid>"

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SpanId(pub String);  // e.g. "span-<uuid>"

// Task-local observability context
task_local! {
    pub static OBS_CONTEXT: Arc<ObservabilityContext>;
}

#[derive(Debug, Clone)]
pub struct ObservabilityContext {
    pub trace_id: TraceId,
    pub current_span_id: SpanId,
    pub user_context: serde_json::Value,  // arbitrary safe metadata
}

#[derive(Debug, Clone)]
pub struct ObservabilityEvent {
    pub event_type: EventType,  // "start" | "finish" | "error"
    pub name: String,           // stable event name, e.g. "phi.agent.turn"
    pub trace_id: TraceId,
    pub span_id: SpanId,
    pub parent_span_id: Option<SpanId>,
    pub timestamp: DateTime<Utc>,
    pub duration_ms: Option<u64>,
    pub context: serde_json::Value,  // safe context metadata
    pub payload: SafePayload,
    pub error: Option<String>,  // safe error category, not full message
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    Start,
    Finish,
    Error,
}
```

### 3.3 trace_operation Function

`trace_operation` is the primary instrumentation entry point. It mirrors Pi's `traceOperation`: read the current context, create a child span, emit a start event, run the callback under the child context, emit a finish/error event.

```rust
pub async fn trace_operation<F, T, E>(
    name: &str,
    payload: SafePayload,
    subscriber: &dyn ObservabilitySubscriber,
    f: F,
) -> Result<T, E>
where
    F: std::future::Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    let parent_ctx = OBS_CONTEXT.try_with(|ctx| (*ctx).clone())
        .unwrap_or_else(|_| Arc::new(ObservabilityContext {
            trace_id: TraceId(uuid::Uuid::new_v4().to_string()),
            current_span_id: SpanId(uuid::Uuid::new_v4().to_string()),
            user_context: serde_json::Value::Null,
        }));

    let child_span_id = SpanId(uuid::Uuid::new_v4().to_string());
    let child_ctx = Arc::new(ObservabilityContext {
        trace_id: parent_ctx.trace_id.clone(),
        current_span_id: child_span_id.clone(),
        user_context: parent_ctx.user_context.clone(),
    });

    let start_event = ObservabilityEvent {
        event_type: EventType::Start,
        name: name.to_string(),
        trace_id: child_ctx.trace_id.clone(),
        span_id: child_span_id.clone(),
        parent_span_id: Some(parent_ctx.current_span_id.clone()),
        timestamp: Utc::now(),
        duration_ms: None,
        context: child_ctx.user_context.clone(),
        payload: payload.clone(),
        error: None,
    };
    subscriber.emit(start_event);

    let start_instant = std::time::Instant::now();
    let result = OBS_CONTEXT.scope(child_ctx, f).await;
    let duration_ms = start_instant.elapsed().as_millis() as u64;

    match &result {
        Ok(_) => {
            let finish_event = ObservabilityEvent {
                event_type: EventType::Finish,
                name: name.to_string(),
                trace_id: child_ctx.trace_id.clone(), // from parent capture
                span_id: child_span_id,
                parent_span_id: Some(parent_ctx.current_span_id.clone()),
                timestamp: Utc::now(),
                duration_ms: Some(duration_ms),
                context: parent_ctx.user_context.clone(),
                payload,
                error: None,
            };
            subscriber.emit(finish_event);
        }
        Err(e) => {
            let error_event = ObservabilityEvent {
                event_type: EventType::Error,
                name: name.to_string(),
                trace_id: parent_ctx.trace_id.clone(),
                span_id: child_span_id,
                parent_span_id: Some(parent_ctx.current_span_id.clone()),
                timestamp: Utc::now(),
                duration_ms: Some(duration_ms),
                context: parent_ctx.user_context.clone(),
                payload,
                error: Some(format!("{}", e)), // safe: error category via Display
            };
            subscriber.emit(error_event);
        }
    }
    result
}
```

### 3.4 Spawning Child Tasks

When spawning a new tokio task (e.g. a batched sub-call), the context must be explicitly passed, since `task_local!` does not cross `tokio::spawn` boundaries:

```rust
let parent_ctx = OBS_CONTEXT.try_with(|c| (*c).clone()).unwrap_or_default();
tokio::spawn(async move {
    OBS_CONTEXT.scope(parent_ctx, async {
        // traced work here
    }).await
});
```

## 4. Stable Event Names and Instrumentation Points

Event names follow the `phi.<area>.<action>` convention. Each is emitted at a specific point in the engine:

| Event Name | Instrumentation Point | Payload (SafePayload) |
|---|---|---|
| `phi.agent.prompt` | `PhiEngine::build_prompt()` — constructs the prompt for a provider call | `{ session_id, message_count }` |
| `phi.agent.turn` | `PhiEngine::run_turn()` — one iteration of the agent loop | `{ session_id, turn_id, consumed_queue_ids_count }` |
| `phi.ai.provider.request` | `PhiEngine::provider_call()` — sends a request to the LLM provider | `{ provider, model, api, session_id }` + finish: `{ stop_reason, token_counts, cost }` |
| `phi.agent.tool_call` | `PhiEngine::execute_tool()` — dispatches a tool (REPL, bash, file ops) | `{ tool_name, call_id }` + finish: `{ success, duration_ms }` |
| `phi.agent.repl` | `PhiEngine::execute_repl()` — executes a REPL code block | `{ block_id, tools_used }` + finish: `{ success, duration_ms }` |
| `phi.agent.exchange` | `PhiEngine::run_exchange()` — wraps a user-to-assistant exchange | `{ session_id, exchange_id }` |
| `phi.operation.compaction` | `PhiHarness::run_compaction()` — context window compaction | `{ strategy, messages_before, messages_after }` |
| `phi.operation.tree` | `PhiHarness::run_tree_op()` — branch/merge tree operation | `{ op_kind, node_count }` |
| `phi.queue.enqueue` | `PhiQueue::enqueue()` — adds an item to the durable queue | `{ queue_id, item_kind }` |
| `phi.queue.consume` | `PhiQueue::consume()` — marks an item consumed by a turn | `{ queue_id, turn_id }` |
| `phi.pending_write.enqueue` | `PhiHarness::enqueue_write()` — buffers a file write | `{ write_id, target_id, target_path }` |
| `phi.pending_write.apply` | `PhiHarness::apply_write()` — applies a buffered write to disk | `{ write_id, target_id, success }` |

### 4.1 Instrumentation Example

```rust
impl PhiEngine {
    async fn run_turn(&self, turn_id: Uuid, queue_ids: Vec<Uuid>) -> Result<TurnResult, PhiError> {
        let payload = SafePayload::json(serde_json::json!({
            "session_id": self.session_id,
            "turn_id": turn_id,
            "consumed_queue_ids_count": queue_ids.len(),
        }));
        trace_operation("phi.agent.turn", payload, self.subscriber.as_ref(), async {
            // ... actual turn logic ...
            // Also append TurnStart/TurnFinish durable entries
        }).await
    }
}
```

## 5. Safe Payload Redaction

### 5.1 Safe vs Unsafe Classification

| Category | Safe (included) | Unsafe (redacted) |
|---|---|---|
| LLM interaction | provider, model, api endpoint, stop\_reason, token\_counts, cost | prompts, completions, system messages |
| Tool execution | tool\_name, call\_id, success, duration\_ms, error\_type | tool args, tool results, shell stdout/stderr, file contents |
| Session | session\_id, turn\_id, message count, leaf node id | message content, user text, assistant text |
| Infrastructure | api (name only, not key), queue\_id, write\_id, target\_path, content\_hash | api keys, secrets, env var values |
| Counts | token counts, cost, durations, message counts | (all counts are safe) |

### 5.2 Redaction Mechanism: SafePayload Type

Phi uses a `SafePayload` newtype that enforces redaction at construction time. It is impossible to accidentally embed unsafe data — the only constructor takes explicitly-typed safe fields.

```rust
/// A payload that has been vetted as safe for external observability consumers.
/// Cannot be constructed from arbitrary JSON — must go through SafePayload::json()
/// which validates against a redaction schema, or SafePayload::empty().
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafePayload {
    fields: serde_json::Map<String, serde_json::Value>,
}

impl SafePayload {
    /// Construct from a JSON value, applying field-level redaction.
    /// Any field matching an unsafe key pattern is replaced with "<redacted>".
    pub fn json(mut value: serde_json::Value) -> Self {
        Self::redact_in_place(&mut value);
        let fields = value.take().as_object().cloned().unwrap_or_default();
        SafePayload { fields }
    }

    pub fn empty() -> Self {
        SafePayload { fields: serde_json::Map::new() }
    }

    /// Recursive redaction: walks the JSON tree and replaces values
    /// for keys matching the unsafe pattern set.
    fn redact_in_place(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, val) in map.iter_mut() {
                    if Self::is_unsafe_key(key) {
                        *val = serde_json::Value::String("<redacted>".to_string());
                    } else {
                        Self::redact_in_place(val);
                    }
                }
            }
            serde_json::Value::Array(arr) => {
                for item in arr {
                    Self::redact_in_place(item);
                }
            }
            _ => {}
        }
    }

    /// Keys (case-insensitive substring match) that indicate unsafe data.
    fn is_unsafe_key(key: &str) -> bool {
        let k = key.to_lowercase();
        const UNSAFE_PATTERNS: &[&str] = &[
            "prompt", "completion", "content", "message_text", "text",
            "args", "arguments", "result", "output", "stdout", "stderr",
            "file_content", "body", "secret", "api_key", "token", "password",
        ];
        UNSAFE_PATTERNS.iter().any(|p| k.contains(p))
    }
}
```

**Key insight**: `SafePayload::is_unsafe_key` uses deny-list matching for defense in depth, but the primary guarantee is that instrumented call sites construct payloads from explicitly safe fields (counts, IDs, durations, enums). The redaction pass is a safety net, not the sole mechanism.

## 6. Subscriber API

### 6.1 Subscriber Trait

External consumers implement the `ObservabilitySubscriber` trait and register with the engine. Events are emitted synchronously by default; async/channel-based subscribers handle their own buffering.

```rust
/// A consumer of observability events. Implementations may forward to
/// OTLP, a file, a TUI, or any external system.
pub trait ObservabilitySubscriber: Send + Sync {
    fn emit(&self, event: ObservabilityEvent);
}

/// A subscriber that does nothing (default when observability is off).
pub struct NoopSubscriber;
impl ObservabilitySubscriber for NoopSubscriber {
    fn emit(&self, _event: ObservabilityEvent) {}
}

/// A subscriber that writes events as JSONL to a file.
pub struct JsonlFileSubscriber {
    writer: Mutex<BufWriter<File>>,
}
impl ObservabilitySubscriber for JsonlFileSubscriber {
    fn emit(&self, event: ObservabilityEvent) {
        let line = serde_json::to_string(&event).unwrap_or_default();
        let mut writer = self.writer.lock().unwrap();
        let _ = writeln!(writer, "{line}");
        let _ = writer.flush();
    }
}
```

### 6.2 Registration

The engine holds an `Arc<dyn ObservabilitySubscriber>`. Multiple subscribers can be composed via a fan-out wrapper:

```rust
/// Fans out events to multiple subscribers.
pub struct FanOutSubscriber {
    subscribers: Vec<Arc<dyn ObservabilitySubscriber>>,
}
impl ObservabilitySubscriber for FanOutSubscriber {
    fn emit(&self, event: ObservabilityEvent) {
        for s in &self.subscribers {
            s.emit(event.clone());
        }
    }
}

impl PhiEngine {
    pub fn with_subscriber(mut self, subscriber: Arc<dyn ObservabilitySubscriber>) -> Self {
        self.subscriber = subscriber;
        self
    }
}
```

### 6.3 Async Channel Subscriber (Backpressure)

For high-throughput or non-blocking consumers, a channel-based subscriber decouples emission from processing:

```rust
pub struct ChannelSubscriber {
    tx: tokio::sync::mpsc::Sender<ObservabilityEvent>,
}
impl ObservabilitySubscriber for ChannelSubscriber {
    fn emit(&self, event: ObservabilityEvent) {
        // try_send: non-blocking. If the channel is full, the event is dropped
        // to prevent observability from blocking the agent loop.
        let _ = self.tx.try_send(event);
    }
}

/// Construct a channel subscriber and return the receiver for the consumer.
pub fn channel_subscriber(
    buffer: usize,
) -> (ChannelSubscriber, tokio::sync::mpsc::Receiver<ObservabilityEvent>) {
    let (tx, rx) = tokio::sync::mpsc::channel(buffer);
    (ChannelSubscriber { tx }, rx)
}
```

**Backpressure policy**: `try_send` is used (non-blocking). If the channel buffer is full, events are dropped. This ensures observability never blocks the agent's critical path. Dropped event counts can be tracked via a `DroppingSubscriber` wrapper that increments an `AtomicU64` counter on drop.

### 6.4 TUI Integration

The existing `RlmEvent` enum for TUI consumption is preserved. A bridge subscriber converts `ObservabilityEvent`s into `RlmEvent`s for the TUI:

```rust
pub struct TuiBridgeSubscriber {
    tui_tx: tokio::sync::mpsc::Sender<RlmEvent>,
}
impl ObservabilitySubscriber for TuiBridgeSubscriber {
    fn emit(&self, event: ObservabilityEvent) {
        if let Some(tui_event) = RlmEvent::from_observability(&event) {
            let _ = self.tui_tx.try_send(tui_event);
        }
    }
}
```
