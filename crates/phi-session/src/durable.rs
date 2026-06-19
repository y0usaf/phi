//! Durable operation/turn/tool-call entries and log-reduction recovery.
//!
//! Additive to the existing JSONL schema: the tolerant reader in
//! [`crate::session::SessionStore`] already skips unknown `type` tags, so
//! these entries can be appended without migrating existing logs. Recovery
//! reduces the raw log text into a [`RecoveryReport`].

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Kind of harness operation recorded by [`DurableEntry::OperationStart`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    Prompt,
    Compaction,
    TreeNavigation,
}

/// Durable entries appended to the session JSONL. Internally tagged by
/// `type` for serde; the tolerant reader skips unknown variants.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DurableEntry {
    OperationStart {
        id: String,
        timestamp: String,
        operation: OperationKind,
        session_id: String,
    },
    OperationFinish {
        id: String,
        timestamp: String,
        operation_id: String,
        stop_cause: Option<String>,
    },
    OperationInterrupted {
        id: String,
        timestamp: String,
        operation_id: String,
        reason: String,
    },
    TurnStart {
        id: String,
        timestamp: String,
        operation_id: String,
        iteration: usize,
        consumed_queue_ids: Vec<String>,
    },
    TurnFinish {
        id: String,
        timestamp: String,
        turn_id: String,
        duration_secs: f64,
    },
    ToolCallStart {
        id: String,
        timestamp: String,
        turn_id: String,
        tool_name: String,
        args_preview: String,
    },
    ToolCallFinish {
        id: String,
        timestamp: String,
        tool_call_id: String,
        result_preview: String,
        error: Option<String>,
    },
    QueueEnqueued {
        id: String,
        timestamp: String,
        queue: String,
        text: String,
    },
    QueueConsumed {
        id: String,
        timestamp: String,
        queue_message_id: String,
        turn_id: String,
    },
    PendingWriteEnqueued {
        id: String,
        timestamp: String,
        entry_type: String,
        entry_data: Value,
    },
    PendingWriteApplied {
        id: String,
        timestamp: String,
        pending_write_id: String,
        target_entry_id: String,
    },
    /// Durable branch-tip marker (set_leaf transition).
    /// Field name `id` matches the existing `LeafRecord` in session.rs so
    /// the reducer can parse leaf entries already in session logs.
    Leaf {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// A model change recorded during an operation.
    ModelChange {
        operation_id: String,
        model: String,
    },
}

/// A queue message reconstructed from the log.
#[derive(Clone, Debug, Default)]
pub struct RestoredQueueMessage {
    pub id: String,
    pub text: String,
    pub source: String,
}

/// A pending write that was enqueued but not yet applied at crash time.
#[derive(Clone, Debug, Default)]
pub struct PendingWrite {
    pub id: String,
    pub entry_type: String,
    pub entry_data: Value,
}

/// An operation that started but never finished.
#[derive(Clone, Debug)]
pub struct UnfinishedOperation {
    pub operation_id: String,
    pub kind: OperationKind,
    pub last_completed_turn: Option<usize>,
}

/// A model change recorded during an operation.
#[derive(Clone, Debug)]
pub struct ModelChange {
    pub operation_id: String,
    pub model: String,
}

/// The result of reducing a session log for recovery.
#[derive(Clone, Debug, Default)]
pub struct RecoveryReport {
    pub current_leaf: Option<String>,
    pub restored_queue_messages: Vec<RestoredQueueMessage>,
    pub pending_writes: Vec<PendingWrite>,
    pub unfinished_operation: Option<UnfinishedOperation>,
    pub model_changes: Vec<ModelChange>,
    pub active_tools: Option<Vec<String>>,
}

/// Tolerant log reducer: parse each line, skip unknown/malformed entries,
/// and reconstruct harness state.
pub struct RecoveryReducer {
    report: RecoveryReport,
    /// operation_id -> (kind, last completed turn iteration)
    open_operations: std::collections::HashMap<String, (OperationKind, Option<usize>)>,
    /// queue message ids that have been consumed.
    consumed_queue_ids: std::collections::HashSet<String>,
}

impl RecoveryReducer {
    pub fn new() -> Self {
        Self {
            report: RecoveryReport::default(),
            open_operations: std::collections::HashMap::new(),
            consumed_queue_ids: std::collections::HashSet::new(),
        }
    }

    /// Process one raw JSONL line. Tolerant: skip malformed/unknown.
    pub fn process(&mut self, line: &str) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return;
        }
        let Ok(entry) = serde_json::from_str::<DurableEntry>(trimmed) else {
            return;
        };
        match entry {
            DurableEntry::OperationStart { id, operation, .. } => {
                self.open_operations.insert(id.clone(), (operation, None));
                self.report.unfinished_operation = Some(UnfinishedOperation {
                    operation_id: id,
                    kind: operation,
                    last_completed_turn: None,
                });
            }
            DurableEntry::OperationFinish { operation_id, .. } => {
                self.open_operations.remove(&operation_id);
                if let Some(uo) = &self.report.unfinished_operation
                    && uo.operation_id == operation_id {
                        self.report.unfinished_operation = None;
                    }
            }
            DurableEntry::OperationInterrupted { operation_id, .. } => {
                self.open_operations.remove(&operation_id);
                // Keep unfinished_operation as None: it was marked interrupted,
                // not left open. The report still records nothing here; callers
                // can inspect the log for interrupted markers.
                if let Some(uo) = &self.report.unfinished_operation
                    && uo.operation_id == operation_id {
                        self.report.unfinished_operation = None;
                    }
            }
            DurableEntry::TurnStart {
                id,
                operation_id,
                iteration,
                ..
            } => {
                if let Some((_, last)) = self.open_operations.get_mut(&operation_id) {
                    *last = Some(iteration);
                }
                if let Some(uo) = self.report.unfinished_operation.as_mut()
                    && uo.operation_id == operation_id {
                        uo.last_completed_turn = Some(iteration);
                    }
                let _ = id;
            }
            DurableEntry::TurnFinish { turn_id, .. } => {
                let _ = turn_id;
            }
            DurableEntry::ToolCallStart { tool_name, .. } => {
                self.report
                    .active_tools
                    .get_or_insert_with(Vec::new)
                    .push(tool_name);
            }
            DurableEntry::ToolCallFinish { .. } => {}
            DurableEntry::QueueEnqueued {
                id, queue, text, ..
            } => {
                if !self.consumed_queue_ids.contains(&id) {
                    self.report
                        .restored_queue_messages
                        .push(RestoredQueueMessage {
                            id,
                            text,
                            source: queue,
                        });
                }
            }
            DurableEntry::QueueConsumed {
                queue_message_id, ..
            } => {
                self.consumed_queue_ids.insert(queue_message_id.clone());
                self.report
                    .restored_queue_messages
                    .retain(|m| m.id != queue_message_id);
            }
            DurableEntry::PendingWriteEnqueued {
                id,
                entry_type,
                entry_data,
                ..
            } => {
                self.report.pending_writes.push(PendingWrite {
                    id,
                    entry_type,
                    entry_data,
                });
            }
            DurableEntry::PendingWriteApplied {
                pending_write_id, ..
            } => {
                self.report
                    .pending_writes
                    .retain(|p| p.id != pending_write_id);
            }
            DurableEntry::Leaf { id } => {
                // Last leaf wins; a None id clears the current leaf.
                self.report.current_leaf = id;
            }
            DurableEntry::ModelChange {
                operation_id,
                model,
            } => {
                self.report.model_changes.push(ModelChange {
                    operation_id,
                    model,
                });
            }
        }
    }

    /// Finalize and return the report.
    pub fn finalize(self) -> RecoveryReport {
        self.report
    }
}

impl Default for RecoveryReducer {
    fn default() -> Self {
        Self::new()
    }
}

/// Serialize a [`DurableEntry`] to a single JSONL line (no trailing newline).
///
/// Returns an error on serialization failure instead of an empty string,
/// so callers can log and handle the failure rather than silently writing
/// a blank line that [`RecoveryReducer`] would skip (invisible data loss).
pub fn to_line(entry: &DurableEntry) -> Result<String, serde_json::Error> {
    serde_json::to_string(entry)
}

/// Reduce a raw session log (the full file contents) into a [`RecoveryReport`].
pub fn reduce_log(text: &str) -> RecoveryReport {
    let mut reducer = RecoveryReducer::new();
    for line in text.lines() {
        reducer.process(line);
    }
    reducer.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op_start(id: &str, op: OperationKind) -> DurableEntry {
        DurableEntry::OperationStart {
            id: id.to_string(),
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            operation: op,
            session_id: "s1".to_string(),
        }
    }

    fn op_finish(op_id: &str) -> DurableEntry {
        DurableEntry::OperationFinish {
            id: "f1".to_string(),
            timestamp: "2024-01-01T00:00:01Z".to_string(),
            operation_id: op_id.to_string(),
            stop_cause: None,
        }
    }

    fn leaf(id: Option<&str>) -> DurableEntry {
        DurableEntry::Leaf {
            id: id.map(|s| s.to_string()),
        }
    }

    fn model_change(op_id: &str, model: &str) -> DurableEntry {
        DurableEntry::ModelChange {
            operation_id: op_id.to_string(),
            model: model.to_string(),
        }
    }

    fn turn_start(op_id: &str, iter: usize) -> DurableEntry {
        DurableEntry::TurnStart {
            id: "t1".to_string(),
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            operation_id: op_id.to_string(),
            iteration: iter,
            consumed_queue_ids: vec![],
        }
    }

    fn reduce(entries: &[DurableEntry]) -> RecoveryReport {
        let log = entries
            .iter()
            .map(|e| to_line(e).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        reduce_log(&log)
    }

    #[test]
    fn reduces_leaf_records_into_current_leaf() {
        let report = reduce(&[
            leaf(Some("leaf-1")),
            leaf(Some("leaf-2")),
        ]);
        assert_eq!(report.current_leaf, Some("leaf-2".to_string()));
    }

    #[test]
    fn clears_leaf_on_none_id() {
        let report = reduce(&[
            leaf(Some("leaf-1")),
            leaf(None),
        ]);
        assert_eq!(report.current_leaf, None);
    }

    #[test]
    fn reduces_model_changes() {
        let report = reduce(&[
            model_change("op-1", "gpt-4o"),
            model_change("op-1", "claude-3.5"),
            model_change("op-2", "gpt-4o"),
        ]);
        assert_eq!(report.model_changes.len(), 3);
        assert_eq!(report.model_changes[0].model, "gpt-4o");
        assert_eq!(report.model_changes[1].model, "claude-3.5");
        assert_eq!(report.model_changes[2].operation_id, "op-2");
    }

    #[test]
    fn end_to_end_recovery_with_mixed_entries() {
        let report = reduce(&[
            op_start("op-1", OperationKind::Prompt),
            model_change("op-1", "gpt-4o"),
            turn_start("op-1", 0),
            leaf(Some("leaf-final")),
            op_finish("op-1"),
            op_start("op-2", OperationKind::Compaction),
        ]);
        // current_leaf from the last Leaf entry
        assert_eq!(report.current_leaf, Some("leaf-final".to_string()));
        // model_changes captured
        assert_eq!(report.model_changes.len(), 1);
        assert_eq!(report.model_changes[0].operation_id, "op-1");
        assert_eq!(report.model_changes[0].model, "gpt-4o");
        // op-1 finished, op-2 is open → unfinished is op-2
        assert!(report.unfinished_operation.is_some());
        assert_eq!(
            report.unfinished_operation.as_ref().unwrap().operation_id,
            "op-2"
        );
    }

    #[test]
    fn tolerant_skips_malformed_lines() {
        let log = r#"{not valid}
{"type":"leaf","id":"good"}
"#;
        let report = reduce_log(log);
        assert_eq!(report.current_leaf, Some("good".to_string()));
    }

    #[test]
    fn empty_lines_are_skipped() {
        let log = r#"
  
{"type":"leaf","id":"x"}

"#;
        let report = reduce_log(log);
        assert_eq!(report.current_leaf, Some("x".to_string()));
    }
}
