# PHASE 4: DURABILITY + OBSERVABILITY

## Part 1: New Durable Entry Types

### Overview

The existing JSONL session schema uses entry types: `session`, `message`, `repl`, `exchange_start`, `exchange_end`, and `leaf`. Phase 4 extends this schema with operation-level, turn-level, tool-call-level, queue-level, and pending-write-level durable entries. Every append remains flushed + `sync_data`'d (disk first, memory second).

### Common Types

```rust
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

pub type EntryId = String;
pub type Timestamp = i64; // unix epoch millis
```

### Existing Entry Structs (updated)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SessionHeader {
    pub session_id: String,
    pub created_at: Timestamp,
    pub phi_version: String,
    pub model: Option<String>,
    pub metadata: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageEntry {
    pub message_id: EntryId,
    pub parent_id: Option<EntryId>,
    pub role: String,
    pub content: String,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplEntry {
    pub repl_id: EntryId,
    pub turn_id: Option<EntryId>,
    pub code: String,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
    pub tools: Vec<String>,
    pub subcalls: Vec<EntryId>,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExchangeStartEntry { pub exchange_id: EntryId, pub timestamp: Timestamp }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExchangeEndEntry { pub exchange_id: EntryId, pub timestamp: Timestamp }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeafEntry { pub message_id: EntryId, pub timestamp: Timestamp }
```

### New: Operation Entries

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct OperationStart {
    pub operation_id: EntryId,
    pub operation_type: OperationType,
    pub parent_operation_id: Option<EntryId>,
    pub timestamp: Timestamp,
    pub payload: Value,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperationType {
    AgentTurn,
    Compaction,
    TreeBranch,
    TreePrune,
    SessionInit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct OperationFinish {
    pub operation_id: EntryId,
    pub timestamp: Timestamp,
    pub result_summary: Option<Value>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct OperationInterrupted {
    pub operation_id: EntryId,
    pub timestamp: Timestamp,
    pub reason: String,
    pub partial_result: Option<Value>,
}
```

### New: Turn Entries

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TurnStart {
    pub turn_id: EntryId,
    pub operation_id: EntryId,
    pub consumed_queue_ids: Vec<EntryId>,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TurnFinish {
    pub turn_id: EntryId,
    pub timestamp: Timestamp,
    pub duration_ms: u64,
    pub stop_reason: Option<String>,
}
```

### New: Tool Call Entries

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ToolCallStart {
    pub tool_call_id: EntryId,
    pub turn_id: EntryId,
    pub tool_name: String,
    pub args_hash: String,
    pub retry_safe: bool,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ToolCallFinish {
    pub tool_call_id: EntryId,
    pub timestamp: Timestamp,
    pub success: bool,
    pub error: Option<String>,
    pub duration_ms: u64,
}
```

### New: Queue Entries

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct QueueEnqueued {
    pub queue_id: EntryId,
    pub operation_id: EntryId,
    pub item_type: String,
    pub payload: Value,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct QueueConsumed {
    pub queue_id: EntryId,
    pub consumed_by_turn_id: EntryId,
    pub timestamp: Timestamp,
}
```

### New: Pending Write Entries

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PendingWriteEnqueued {
    pub write_id: EntryId,
    pub target_path: String,
    pub content_hash: String,
    pub content_length: u64,
    pub operation_id: EntryId,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PendingWriteApplied {
    pub write_id: EntryId,
    pub timestamp: Timestamp,
    pub bytes_written: u64,
    pub success: bool,
}
```

### SessionEntry Enum (tags all variants)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEntry {
    Session(SessionHeader),
    Message(MessageEntry),
    Repl(ReplEntry),
    ExchangeStart(ExchangeStartEntry),
    ExchangeEnd(ExchangeEndEntry),
    Leaf(LeafEntry),
    OperationStart(OperationStart),
    OperationFinish(OperationFinish),
    OperationInterrupted(OperationInterrupted),
    TurnStart(TurnStart),
    TurnFinish(TurnFinish),
    ToolCallStart(ToolCallStart),
    ToolCallFinish(ToolCallFinish),
    QueueEnqueued(QueueEnqueued),
    QueueConsumed(QueueConsumed),
    PendingWriteEnqueued(PendingWriteEnqueued),
    PendingWriteApplied(PendingWriteApplied),
}
```

### Relationships to Existing Entries

- **OperationStart/Finish/Interrupted** wrap coarse-grained agent operations (agent turns, compaction, tree ops). `parent_operation_id` allows nesting.
- **TurnStart/Finish** live within an operation. `consumed_queue_ids` links to `QueueEnqueued` entries, establishing the queue-to-turn dependency chain.
- **ToolCallStart/Finish** reference a `turn_id`, providing tool-call-level durability inside a turn. `retry_safe` flag drives recovery policy.
- **QueueEnqueued/Consumed** model the durable work queue. `operation_id` ties items to the operation that enqueued them.
- **PendingWriteEnqueued/Applied** provide deterministic write tracking via `write_id` and `content_hash` for idempotent application.
- Existing `exchange_start`/`exchange_end` remain as coarse turn boundaries; `turn_start`/`turn_finish` provide finer-grained operation-level tracking within exchanges.
- `message` entries remain the conversation DAG. `repl` entries remain code execution records but now optionally reference a `turn_id`.
- `leaf` entries continue to mark branch tips.

---

## Part 2: Recovery Logic

### RecoveryState Struct

```rust
#[derive(Debug, Default)]
pub struct RecoveryState {
    pub session: Option<SessionHeader>,
    pub messages: Vec<MessageEntry>,
    pub repls: Vec<ReplEntry>,
    pub leaf: Option<LeafEntry>,
    pub operations: HashMap<EntryId, OperationStatus>,
    pub turns: HashMap<EntryId, TurnStatus>,
    pub tool_calls: HashMap<EntryId, ToolCallStatus>,
    pub queue: HashMap<EntryId, QueueItemStatus>,
    pub pending_writes: HashMap<EntryId, PendingWriteStatus>,
    pub interrupted_exchanges: Vec<EntryId>,
    pub errors: Vec<RecoveryError>,
}

#[derive(Debug, Clone)]
pub enum OperationStatus {
    Started { op_type: OperationType, payload: Value },
    Finished { result: Option<Value> },
    Interrupted { reason: String },
}

#[derive(Debug, Clone)]
pub enum TurnStatus {
    InProgress { operation_id: EntryId, consumed_queue_ids: Vec<EntryId> },
    Completed { duration_ms: u64 },
}

#[derive(Debug, Clone)]
pub enum ToolCallStatus {
    Started { turn_id: EntryId, tool_name: String, retry_safe: bool },
    Completed { success: bool, error: Option<String> },
}

#[derive(Debug, Clone)]
pub struct QueueItemStatus {
    pub item_type: String,
    pub payload: Value,
    pub consumed_by: Option<EntryId>,
}

#[derive(Debug, Clone)]
pub enum PendingWriteStatus {
    Enqueued { target_path: String, content_hash: String },
    Applied { bytes_written: u64, success: bool },
}

#[derive(Debug, Clone)]
pub enum RecoveryError {
    MissingOperation(EntryId, String),
    UnknownOperationFinish(EntryId),
    DanglingTurnStart(EntryId),
    UnconsumedQueueItem(EntryId),
    UnappliedPendingWrite(EntryId, String),
    DeserializationError(String, usize),
    MissingDependency(String),
    Io(String),
}
```

### Recovery Algorithm: Log Reduction

`PhiHarness::recover()` reads the JSONL log line-by-line, deserializes each line into a `SessionEntry`, and folds it into `RecoveryState`. After the fold, a reconciliation phase marks unfinished operations and a validation phase checks runtime dependencies.

```rust
impl PhiHarness {
    pub fn recover(&self, log_path: &Path) -> Result<RecoveryState, RecoveryError> {
        let file = File::open(log_path)
            .map_err(|e| RecoveryError::Io(e.to_string()))?;
        let reader = BufReader::new(file);
        let mut state = RecoveryState::default();

        for (line_num, line) in reader.lines().enumerate() {
            let line = line
                .map_err(|e| RecoveryError::DeserializationError(e.to_string(), line_num))?;
            if line.trim().is_empty() { continue; }
            let entry: SessionEntry = serde_json::from_str(&line)
                .map_err(|e| RecoveryError::DeserializationError(e.to_string(), line_num))?;

            match entry {
                SessionEntry::Session(h) => state.session = Some(h),
                SessionEntry::Message(m) => state.messages.push(m),
                SessionEntry::Repl(r) => state.repls.push(r),
                SessionEntry::Leaf(l) => state.leaf = Some(l),
                SessionEntry::ExchangeStart(e) => {
                    state.interrupted_exchanges.push(e.exchange_id);
                }
                SessionEntry::ExchangeEnd(e) => {
                    state.interrupted_exchanges.retain(|id| *id != e.exchange_id);
                }
                SessionEntry::OperationStart(op) => {
                    state.operations.insert(op.operation_id.clone(),
                        OperationStatus::Started {
                            op_type: op.operation_type,
                            payload: op.payload,
                        });
                }
                SessionEntry::OperationFinish(op) => {
                    let existing = state.operations.get(&op.operation_id)
                        .ok_or(RecoveryError::UnknownOperationFinish(op.operation_id.clone()))?;
                    *existing = OperationStatus::Finished { result: op.result_summary };
                }
                SessionEntry::OperationInterrupted(op) => {
                    if let Some(o) = state.operations.get_mut(&op.operation_id) {
                        *o = OperationStatus::Interrupted { reason: op.reason };
                    }
                }
                SessionEntry::TurnStart(t) => {
                    state.turns.insert(t.turn_id.clone(),
                        TurnStatus::InProgress {
                            operation_id: t.operation_id,
                            consumed_queue_ids: t.consumed_queue_ids,
                        });
                }
                SessionEntry::TurnFinish(t) => {
                    if let Some(turn) = state.turns.get_mut(&t.turn_id) {
                        *turn = TurnStatus::Completed { duration_ms: t.duration_ms };
                    }
                }
                SessionEntry::ToolCallStart(tc) => {
                    state.tool_calls.insert(tc.tool_call_id.clone(),
                        ToolCallStatus::Started {
                            turn_id: tc.turn_id,
                            tool_name: tc.tool_name,
                            retry_safe: tc.retry_safe,
                        });
                }
                SessionEntry::ToolCallFinish(tc) => {
                    if let Some(tcs) = state.tool_calls.get_mut(&tc.tool_call_id) {
                        *tcs = ToolCallStatus::Completed {
                            success: tc.success,
                            error: tc.error,
                        };
                    }
                }
                SessionEntry::QueueEnqueued(q) => {
                    state.queue.insert(q.queue_id.clone(),
                        QueueItemStatus {
                            item_type: q.item_type,
                            payload: q.payload,
                            consumed_by: None,
                        });
                }
                SessionEntry::QueueConsumed(q) => {
                    if let Some(item) = state.queue.get_mut(&q.queue_id) {
                        item.consumed_by = Some(q.consumed_by_turn_id);
                    }
                }
                SessionEntry::PendingWriteEnqueued(w) => {
                    state.pending_writes.insert(w.write_id.clone(),
                        PendingWriteStatus::Enqueued {
                            target_path: w.target_path,
                            content_hash: w.content_hash,
                        });
                }
                SessionEntry::PendingWriteApplied(w) => {
                    if let Some(pw) = state.pending_writes.get_mut(&w.write_id) {
                        *pw = PendingWriteStatus::Applied {
                            bytes_written: w.bytes_written,
                            success: w.success,
                        };
                    }
                }
            }
        }

        // Post-replay reconciliation
        self.reconcile_unfinished(&mut state);
        // Validate runtime dependencies
        self.validate_runtime_deps(&state)?;
        Ok(state)
    }
}
```

### Reconciliation: Unfinished Operations

After the fold, any entry in a `Started`/`InProgress`/`Enqueued` state without a corresponding `Finish`/`Consumed`/`Applied` entry is unfinished. The reconciliation phase applies conservative policies:

```rust
impl PhiHarness {
    fn reconcile_unfinished(&self, state: &mut RecoveryState) {
        // 1. Unfinished operations: mark interrupted
        for (op_id, status) in state.operations.iter_mut() {
            if let OperationStatus::Started { op_type, .. } = status {
                *status = OperationStatus::Interrupted {
                    reason: format!("{:?} operation unfinished at recovery", op_type),
                };
            }
        }

        // 2. Unfinished turns: flag as dangling
        for (turn_id, status) in state.turns.iter() {
            if matches!(status, TurnStatus::InProgress { .. }) {
                state.errors.push(RecoveryError::DanglingTurnStart(turn_id.clone()));
            }
        }

        // 3. Unfinished tool calls: do NOT retry unless retry_safe
        for (tc_id, status) in state.tool_calls.iter() {
            if let ToolCallStatus::Started { retry_safe: false, .. } = status {
                state.errors.push(
                    RecoveryError::UnappliedPendingWrite(
                        tc_id.clone(),
                        "non-retry-safe tool call unfinished".into(),
                    )
                );
            }
            // retry_safe == true: eligible for re-dispatch by caller
        }

        // 4. Unapplied pending writes: flag for idempotent re-apply or review
        for (w_id, pw) in state.pending_writes.iter() {
            if let PendingWriteStatus::Enqueued { target_path, content_hash } = pw {
                state.errors.push(
                    RecoveryError::UnappliedPendingWrite(w_id.clone(), target_path.clone())
                );
            }
        }

        // 5. Unconsumed queue items: carry forward as pending
        for (q_id, item) in state.queue.iter() {
            if item.consumed_by.is_none() {
                state.errors.push(RecoveryError::UnconsumedQueueItem(q_id.clone()));
            }
        }
    }
}
```

### Runtime Dependency Validation

```rust
impl PhiHarness {
    fn validate_runtime_deps(&self, state: &RecoveryState) -> Result<(), RecoveryError> {
        // Session header must exist
        let session = state.session.as_ref()
            .ok_or(RecoveryError::MissingDependency("session header".into()))?;

        // Model availability
        if let Some(ref model) = session.model {
            if !self.available_models().contains(model) {
                return Err(RecoveryError::MissingDependency(
                    format!("model: {}", model)));
            }
        }

        // Working directory exists
        if !self.working_dir().exists() {
            return Err(RecoveryError::MissingDependency("working directory".into()));
        }

        // Message DAG integrity: every parent_id must exist
        let msg_ids: HashSet<_> = state.messages.iter()
            .map(|m| &m.message_id).collect();
        for msg in &state.messages {
            if let Some(ref parent) = msg.parent_id {
                if !msg_ids.contains(parent) {
                    state.errors.push(RecoveryError::MissingOperation(
                        msg.message_id.clone(),
                        format!("parent message {} not found", parent)));
                }
            }
        }

        // Turn-to-queue linking: every consumed_queue_id must exist
        for (turn_id, status) in &state.turns {
            if let TurnStatus::InProgress { consumed_queue_ids, .. } = status {
                for q_id in consumed_queue_ids {
                    if !state.queue.contains_key(q_id) {
                        state.errors.push(RecoveryError::MissingOperation(
                            turn_id.clone(),
                            format!("queue item {} not found", q_id)));
                    }
                }
            }
        }

        Ok(())
    }
}
```

### Reconstructed State Mapping

After recovery, `RecoveryState` maps to in-memory session state:

- **session** -> `PhiSession` header (ID, model, metadata)
- **messages** -> conversation DAG nodes (injected as `history_N` variables during replay; code is NOT re-executed)
- **repls** -> attached as context, not re-run
- **operations** -> live `OperationTracker` map; interrupted ops are surfaced to the caller for decision
- **turns** -> in-progress turns are abandoned (marked interrupted); completed turns are logged
- **tool_calls** -> non-retry-safe unfinished calls are reported as errors; retry-safe ones are eligible for re-dispatch
- **queue** -> unconsumed items become `pending_queue` for the next turn
- **pending_writes** -> unapplied writes are collected for idempotent re-apply (verified via `content_hash`) or manual review
- **interrupted_exchanges** -> surfaced as incomplete turns to the user

### Conservative Interruption Policy Summary

| Scenario | Action |
|---|---|
| Operation started, no finish/interrupted | Mark `OperationInterrupted`; do not auto-retry |
| Compaction/tree op unfinished, no final entry | Eligible for re-run only if no `OperationFinish` exists in log |
| Tool call started, no finish, not retry-safe | Do NOT retry; report as error |
| Tool call started, no finish, retry-safe | Eligible for re-dispatch by caller |
| Turn started, no finish | Mark interrupted; do not continue partial turn |
| Queue item enqueued, never consumed | Carry forward to `pending_queue` |
| Pending write enqueued, never applied | Re-apply idempotently via `content_hash`, or flag for review |
| Exchange start without exchange end | Mark as interrupted exchange |

### Turn-to-Queue Linking Detail

When `TurnStart` is written, its `consumed_queue_ids` field records exactly which `QueueEnqueued` entries this turn consumed. During recovery:

1. For each `TurnStart`, resolve each `queue_id` in `consumed_queue_ids` against the `queue` map in `RecoveryState`.
2. If a `QueueConsumed` entry exists for that ID, verify `consumed_by_turn_id` matches the `TurnStart`'s `turn_id`.
3. If the queue entry is missing or the `consumed_by_turn_id` mismatches, push a `RecoveryError::MissingOperation`.
4. Unconsumed queue items (enqueued but no `QueueConsumed`) are carried forward as pending work for the next agent turn.

### Compaction/Tree Op Retry Policy

Compaction and tree operations (`Compaction`, `TreeBranch`, `TreePrune`) are re-run during recovery **only** when:

1. An `OperationStart` entry exists for the operation.
2. No `OperationFinish` entry exists for the same `operation_id`.
3. The operation is not already marked `OperationInterrupted`.

When re-running is warranted, the caller retrieves the original `payload` from the `OperationStart` entry and re-dispatches. If a `OperationFinish` entry already exists, the operation is assumed complete and its `result_summary` is used to reconstruct state without re-execution.