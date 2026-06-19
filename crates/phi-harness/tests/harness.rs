//! Integration tests for phi-harness: queues, loop detection, hooks,
//! observability, and durable recovery.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use phi_harness::{
    ContextEvent, ContextResult, HookHandler, HookObserver, ObservabilityEvent,
    ObservabilitySubscriber, PhiHooks, QueueSource, Queues, SourceInfo, ToolCallEvent,
    ToolCallResult, ToolResultEvent, ToolResultPatch, safe_payload, subscribe_observability,
    trace_operation,
};
use phi_rlm::LoopDetector;
use phi_session::durable::{DurableEntry, OperationKind, RecoveryReducer, reduce_log};
use serde_json::json;

// ── Queue tests ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn queue_steer_enqueue_and_drain() {
    let q = Queues::new();
    let id1 = q.enqueue_steer("go left").await;
    let id2 = q.enqueue_steer("go right").await;
    assert!(!id1.is_empty());
    assert!(!id2.is_empty());

    let drained = q.drain_at_save_point().await;
    assert_eq!(drained.len(), 2);
    assert_eq!(drained[0].text, "go left");
    assert_eq!(drained[1].text, "go right");
    assert_eq!(drained[0].source, QueueSource::Steer);

    // Drain again: should be empty.
    let drained2 = q.drain_at_save_point().await;
    assert!(drained2.is_empty());
}

#[tokio::test]
async fn queue_follow_up_and_steer_drained_together() {
    let q = Queues::new();
    q.enqueue_steer("steer1").await;
    q.enqueue_follow_up("follow1").await;

    let drained = q.drain_at_save_point().await;
    assert_eq!(drained.len(), 2);
    // Steer comes first, then follow-up.
    assert_eq!(drained[0].source, QueueSource::Steer);
    assert_eq!(drained[1].source, QueueSource::FollowUp);
}

#[tokio::test]
async fn queue_next_turn_survives_clear_runtime() {
    let q = Queues::new();
    q.enqueue_steer("steer").await;
    q.enqueue_next_turn("next").await;

    q.clear_runtime_queues().await;

    // Steer should be cleared, next_turn should survive.
    let steer_snap = q.steer_snapshot().await;
    assert!(steer_snap.is_empty());

    let next = q.drain_next_turn().await;
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].text, "next");
    assert_eq!(next[0].source, QueueSource::NextTurn);
}

#[tokio::test]
async fn queue_mode_setters() {
    let q = Queues::new();
    q.set_steer_mode(phi_harness::QueueMode::Accumulate).await;
    q.set_follow_up_mode(phi_harness::QueueMode::Interleave)
        .await;
    // Modes are set; no panic.
}

// ── Loop detector tests ─────────────────────────────────────────────────────

#[test]
fn loop_detector_no_repeat() {
    let mut det = LoopDetector::new();
    let args = json!({"key": "value"});
    for i in 0..3 {
        let result = format!("result_{i}");
        assert!(det.observe("bash", &args, &result).is_none());
    }
}

#[test]
fn loop_detector_triggers_on_repeat() {
    let mut det = LoopDetector::with_params(10, 3);
    let args = json!({"cmd": "ls"});
    let result = "same output";

    // max_repeats=3, threshold is exclusive (> 3), so 4th identical call triggers.
    for _ in 0..3 {
        assert!(det.observe("bash", &args, result).is_none());
    }
    assert!(det.observe("bash", &args, result).is_some());
}

#[test]
fn loop_detector_window_evicts_old_signatures() {
    let mut det = LoopDetector::with_params(3, 5); // small window
    let args = json!(null);

    // Fill window with unique signatures.
    for i in 0..5 {
        assert!(det.observe("tool", &args, &format!("out_{i}")).is_none());
    }
    // Old entries evicted; repeating "out_0" shouldn't loop.
    assert!(det.observe("tool", &args, "out_0").is_none());
}

#[test]
fn loop_detector_reset() {
    let mut det = LoopDetector::with_params(5, 2);
    let args = json!(null);
    for _ in 0..3 {
        let _ = det.observe("t", &args, "r");
    }
    det.reset();
    // After reset, same call shouldn't immediately trigger.
    assert!(det.observe("t", &args, "r").is_none());
}

// ── Hooks tests ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn hooks_emit_context_chains_messages() {
    let hooks = PhiHooks::new();
    let handler1: HookHandler<ContextEvent> = Arc::new(|ev: &ContextEvent| {
        Ok(Some(ContextResult {
            messages: Some({
                let mut msgs = ev.messages.clone();
                msgs.push(phi_rlm::ChatMessage {
                    role: phi_rlm::Role::User,
                    content: "from handler1".to_owned(),
                });
                msgs
            }),
        }))
    });
    let handler2: HookHandler<ContextEvent> = Arc::new(|ev: &ContextEvent| {
        Ok(Some(ContextResult {
            messages: Some({
                let mut msgs = ev.messages.clone();
                msgs.push(phi_rlm::ChatMessage {
                    role: phi_rlm::Role::User,
                    content: "from handler2".to_owned(),
                });
                msgs
            }),
        }))
    });
    let _ = hooks
        .on::<ContextEvent>(SourceInfo::default(), handler1)
        .await;
    let _ = hooks
        .on::<ContextEvent>(SourceInfo::default(), handler2)
        .await;

    let event = ContextEvent {
        messages: vec![phi_rlm::ChatMessage {
            role: phi_rlm::Role::User,
            content: "initial".to_owned(),
        }],
    };
    let result = hooks.emit_context(event).await.unwrap();
    assert!(result.is_some());
    let messages = result.unwrap().messages.unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0].content, "initial");
    assert_eq!(messages[1].content, "from handler1");
    assert_eq!(messages[2].content, "from handler2");
}

#[tokio::test]
async fn hooks_tool_call_early_exit_on_block() {
    let hooks = PhiHooks::new();

    let handler1: HookHandler<ToolCallEvent> = Arc::new(|_| {
        Ok(Some(ToolCallResult {
            block: false,
            reason: None,
        }))
    });
    let handler2: HookHandler<ToolCallEvent> = Arc::new(|_| {
        Ok(Some(ToolCallResult {
            block: true,
            reason: Some("blocked by handler2".to_owned()),
        }))
    });
    let handler3: HookHandler<ToolCallEvent> = Arc::new(|_| {
        Ok(Some(ToolCallResult {
            block: true,
            reason: Some("should not reach handler3".to_owned()),
        }))
    });
    let _ = hooks
        .on::<ToolCallEvent>(SourceInfo::default(), handler1)
        .await;
    let _ = hooks
        .on::<ToolCallEvent>(SourceInfo::default(), handler2)
        .await;
    let _ = hooks
        .on::<ToolCallEvent>(SourceInfo::default(), handler3)
        .await;

    let event = ToolCallEvent {
        tool_name: "bash".to_owned(),
        input: json!({"cmd": "rm -rf /"}),
    };
    let result = hooks.emit_tool_call(event).await.unwrap();
    assert!(result.is_some());
    let r = result.unwrap();
    assert!(r.block);
    assert_eq!(r.reason.as_deref(), Some("blocked by handler2"));
}

#[tokio::test]
async fn hooks_tool_result_patch_accumulation() {
    let hooks = PhiHooks::new();

    let handler1: HookHandler<ToolResultEvent> = Arc::new(|ev: &ToolResultEvent| {
        Ok(Some(ToolResultPatch {
            content: Some(format!("{} [patched]", ev.content)),
            is_error: None,
        }))
    });
    let handler2: HookHandler<ToolResultEvent> = Arc::new(|_ev: &ToolResultEvent| {
        Ok(Some(ToolResultPatch {
            content: None,
            is_error: Some(true),
        }))
    });
    let _ = hooks
        .on::<ToolResultEvent>(SourceInfo::default(), handler1)
        .await;
    let _ = hooks
        .on::<ToolResultEvent>(SourceInfo::default(), handler2)
        .await;

    let event = ToolResultEvent {
        tool_name: "bash".to_owned(),
        content: "original".to_owned(),
        is_error: false,
    };
    let result = hooks.emit_tool_result(event).await.unwrap();
    assert!(result.is_some());
    let patch = result.unwrap();
    assert_eq!(patch.content.as_deref(), Some("original [patched]"));
    assert_eq!(patch.is_error, Some(true));
}

#[tokio::test]
async fn hooks_observation_runs_observers_and_handlers() {
    let hooks = PhiHooks::new();
    let counter = Arc::new(AtomicUsize::new(0));
    let c1 = counter.clone();

    let observer: HookObserver = Arc::new(move |_ev: &dyn std::any::Any| {
        c1.fetch_add(1, Ordering::Relaxed);
        Ok(())
    });
    let _ = hooks.observe(observer).await;

    hooks
        .emit_observation(phi_harness::RunStartEvent {
            session_id: "test".to_owned(),
        })
        .await
        .unwrap();

    assert_eq!(counter.load(Ordering::Relaxed), 1);
}

// ── Observability tests ─────────────────────────────────────────────────────

#[tokio::test]
async fn observability_subscribe_and_trace() {
    let received = Arc::new(AtomicUsize::new(0));
    let r = received.clone();
    let sub: ObservabilitySubscriber = Arc::new(move |_ev: &ObservabilityEvent| {
        r.fetch_add(1, Ordering::Relaxed);
    });
    let _cleanup = subscribe_observability(sub);

    let result = trace_operation("phi.test.operation", json!({"key": "val"}), || async {
        Ok::<_, phi_harness::HarnessError>(42)
    })
    .await
    .unwrap();

    assert_eq!(result, 42);
    // Should have received at least Start + End events.
    assert!(received.load(Ordering::Relaxed) >= 2);
}

#[tokio::test]
async fn observability_trace_error() {
    let received = Arc::new(AtomicUsize::new(0));
    let r = received.clone();
    let sub: ObservabilitySubscriber = Arc::new(move |_ev: &ObservabilityEvent| {
        r.fetch_add(1, Ordering::Relaxed);
    });
    let _cleanup = subscribe_observability(sub);

    let result: Result<i32, phi_harness::HarnessError> =
        trace_operation("phi.test.error", json!({}), || async {
            Err(phi_harness::HarnessError::Busy {
                phase: phi_harness::HarnessPhase::Turn,
            })
        })
        .await;

    assert!(result.is_err());
    // Should have received Start + Error events.
    assert!(received.load(Ordering::Relaxed) >= 2);
}

#[test]
fn safe_payload_redacts_unsafe_keys() {
    let payload = json!({
        "model": "gpt-4",
        "prompt": "secret prompt",
        "api_key": "sk-xxx",
        "input_tokens": 100,
    });
    let safe = safe_payload("phi.ai.provider.request", &payload);
    let obj = safe.as_object().unwrap();
    assert!(obj.contains_key("model"));
    assert!(obj.contains_key("input_tokens"));
    assert!(!obj.contains_key("prompt"));
    assert!(!obj.contains_key("api_key"));
}

// ── Durable recovery tests ──────────────────────────────────────────────────

#[test]
fn recovery_reducer_unfinished_operation() {
    let mut reducer = RecoveryReducer::new();
    let start = serde_json::to_string(&DurableEntry::OperationStart {
        id: "op1".to_owned(),
        timestamp: "2024-01-01T00:00:00Z".to_owned(),
        operation: OperationKind::Prompt,
        session_id: "s1".to_owned(),
    })
    .unwrap();
    reducer.process(&start);

    let turn = serde_json::to_string(&DurableEntry::TurnStart {
        id: "t1".to_owned(),
        timestamp: "2024-01-01T00:00:01Z".to_owned(),
        operation_id: "op1".to_owned(),
        iteration: 1,
        consumed_queue_ids: vec![],
    })
    .unwrap();
    reducer.process(&turn);

    let report = reducer.finalize();
    assert!(report.unfinished_operation.is_some());
    let uo = report.unfinished_operation.unwrap();
    assert_eq!(uo.operation_id, "op1");
    assert_eq!(uo.kind, OperationKind::Prompt);
    assert_eq!(uo.last_completed_turn, Some(1));
}

#[test]
fn recovery_reducer_finished_operation_clears_unfinished() {
    let text = format!(
        "{}\n{}",
        serde_json::to_string(&DurableEntry::OperationStart {
            id: "op1".to_owned(),
            timestamp: "t".to_owned(),
            operation: OperationKind::Prompt,
            session_id: "s".to_owned(),
        })
        .unwrap(),
        serde_json::to_string(&DurableEntry::OperationFinish {
            id: "f1".to_owned(),
            timestamp: "t".to_owned(),
            operation_id: "op1".to_owned(),
            stop_cause: Some("Answer".to_owned()),
        })
        .unwrap()
    );
    let report = reduce_log(&text);
    assert!(report.unfinished_operation.is_none());
}

#[test]
fn recovery_reducer_restores_unconsumed_queue_messages() {
    let text = format!(
        "{}\n{}",
        serde_json::to_string(&DurableEntry::QueueEnqueued {
            id: "q1".to_owned(),
            timestamp: "t".to_owned(),
            queue: "steer".to_owned(),
            text: "go left".to_owned(),
        })
        .unwrap(),
        serde_json::to_string(&DurableEntry::QueueEnqueued {
            id: "q2".to_owned(),
            timestamp: "t".to_owned(),
            queue: "next_turn".to_owned(),
            text: "next msg".to_owned(),
        })
        .unwrap()
    );
    let report = reduce_log(&text);
    assert_eq!(report.restored_queue_messages.len(), 2);
    assert_eq!(report.restored_queue_messages[0].text, "go left");
    assert_eq!(report.restored_queue_messages[1].source, "next_turn");
}

#[test]
fn recovery_reducer_consumed_queue_removed() {
    let text = format!(
        "{}\n{}\n{}",
        serde_json::to_string(&DurableEntry::QueueEnqueued {
            id: "q1".to_owned(),
            timestamp: "t".to_owned(),
            queue: "steer".to_owned(),
            text: "consumed msg".to_owned(),
        })
        .unwrap(),
        serde_json::to_string(&DurableEntry::QueueConsumed {
            id: "c1".to_owned(),
            timestamp: "t".to_owned(),
            queue_message_id: "q1".to_owned(),
            turn_id: "t1".to_owned(),
        })
        .unwrap(),
        serde_json::to_string(&DurableEntry::QueueEnqueued {
            id: "q2".to_owned(),
            timestamp: "t".to_owned(),
            queue: "follow_up".to_owned(),
            text: "unconsumed".to_owned(),
        })
        .unwrap()
    );
    let report = reduce_log(&text);
    assert_eq!(report.restored_queue_messages.len(), 1);
    assert_eq!(report.restored_queue_messages[0].id, "q2");
}

#[test]
fn recovery_reducer_pending_writes() {
    let text = serde_json::to_string(&DurableEntry::PendingWriteEnqueued {
        id: "pw1".to_owned(),
        timestamp: "t".to_owned(),
        entry_type: "message".to_owned(),
        entry_data: json!({"text": "deferred"}),
    })
    .unwrap();
    let report = reduce_log(&text);
    assert_eq!(report.pending_writes.len(), 1);
    assert_eq!(report.pending_writes[0].id, "pw1");
    assert_eq!(report.pending_writes[0].entry_type, "message");
}

#[test]
fn recovery_reducer_pending_write_applied_removes_entry() {
    let text = format!(
        "{}\n{}",
        serde_json::to_string(&DurableEntry::PendingWriteEnqueued {
            id: "pw1".to_owned(),
            timestamp: "t".to_owned(),
            entry_type: "message".to_owned(),
            entry_data: json!({"text": "deferred"}),
        })
        .unwrap(),
        serde_json::to_string(&DurableEntry::PendingWriteApplied {
            id: "pwa1".to_owned(),
            timestamp: "t".to_owned(),
            pending_write_id: "pw1".to_owned(),
            target_entry_id: "e1".to_owned(),
        })
        .unwrap()
    );
    let report = reduce_log(&text);
    assert!(report.pending_writes.is_empty());
}

#[test]
fn recovery_reducer_skips_malformed_lines() {
    let text = "not json\n{}\n\n";
    let report = reduce_log(text);
    assert!(report.restored_queue_messages.is_empty());
    assert!(report.pending_writes.is_empty());
    assert!(report.unfinished_operation.is_none());
}

#[test]
fn recovery_reducer_tool_call_records_active_tools() {
    let text = serde_json::to_string(&DurableEntry::ToolCallStart {
        id: "tc1".to_owned(),
        timestamp: "t".to_owned(),
        turn_id: "t1".to_owned(),
        tool_name: "bash".to_owned(),
        args_preview: "ls".to_owned(),
    })
    .unwrap();
    let report = reduce_log(&text);
    assert_eq!(report.active_tools.unwrap().len(), 1);
}
