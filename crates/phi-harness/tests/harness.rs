//! Integration tests for phi-harness: queues, loop detection, hooks,
//! observability, and session log recovery.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use phi_harness::{
    ContextEvent, ContextResult, HookHandler, HookObserver, ObservabilityEvent,
    ObservabilitySubscriber, PhiHooks, QueueSource, Queues, SourceInfo, ToolCallEvent,
    ToolCallResult, ToolResultEvent, ToolResultPatch, safe_payload, subscribe_observability,
    trace_operation,
};
use phi_rlm::LoopDetector;
use phi_session::session::SessionStore;
use serde_json::json;
use tempfile::tempdir;

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
    // Three distinct steps (different results) should never trigger.
    let mut det = LoopDetector::with_params(10, 5);
    for i in 0..3 {
        det.observe_call("bash", &json!({"key": "value"}), &format!("result_{i}"));
        assert!(det.finalize_step().is_none());
    }
}

#[test]
fn loop_detector_triggers_on_repeat() {
    // max_repeats=3, threshold is exclusive (> 3), so 4th identical step triggers
    // once the window has 10 steps.
    let mut det = LoopDetector::with_params(10, 3);
    let args = json!({"cmd": "ls"});
    let result = "same output";

    // Push 6 identical steps (window too small to fire yet).
    for _ in 0..6 {
        det.observe_call("bash", &args, result);
        assert!(det.finalize_step().is_none());
    }
    // Fill window with 3 distinct steps to reach 9 total (still < window).
    for i in 0..3 {
        det.observe_call("tool", &args, &format!("out_{i}"));
        assert!(det.finalize_step().is_none());
    }
    // 10th step is the 7th identical -> window full, count=7 > 3 -> detected.
    det.observe_call("bash", &args, result);
    assert!(det.finalize_step().is_some());
}

#[test]
fn loop_detector_window_evicts_old_signatures() {
    // With a small window (3), old entries scroll out. 5 distinct steps + 1
    // repeat of step 0 means only 3 in window at detection time, and the
    // repeat is not among them.
    let mut det = LoopDetector::with_params(3, 5);
    let args = json!(null);

    for i in 0..5 {
        det.observe_call("tool", &args, &format!("out_{i}"));
        assert!(det.finalize_step().is_none());
    }
    // "out_0" has scrolled out of the window; repeating it shouldn't loop.
    det.observe_call("tool", &args, "out_0");
    assert!(det.finalize_step().is_none());
}

#[test]
fn loop_detector_reset() {
    let mut det = LoopDetector::with_params(5, 2);
    let args = json!(null);
    for _ in 0..3 {
        det.observe_call("t", &args, "r");
    }
    let _ = det.finalize_step();
    det.reset();
    // After reset, history is empty; same call shouldn't trigger.
    det.observe_call("t", &args, "r");
    assert!(det.finalize_step().is_none());
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
// ── Session log recovery tests ──────────────────────────────────────────────
//
// The durable layer was pruned: the session log's own tolerant reader
// (SessionStore::load) already handles unknown types, malformed lines, and
// leaf reduction. These tests verify those properties through the session
// store directly, not through a separate reducer.

#[test]
fn session_log_tolerates_unknown_entry_types() {
    // Unknown types (e.g. operation_start from an older log) are skipped
    // by the tolerant reader, not errors. The leaf falls back to the last
    // valid message entry.
    let dir = tempdir().unwrap();
    let path = dir.path().join("test.jsonl");
    let log = r#"{"type":"session","id":"s1","timestamp":"2024-01-01T00:00:00Z","cwd":"/tmp"}
{"type":"operation_start","id":"op-1","operation":"prompt"}
{"type":"message","id":"msg-1","parent_id":null,"timestamp":"2024-01-01T00:00:00Z","role":"user","text":"hello"}
{"type":"queue_enqueued","id":"q1","queue":"steer","text":"go left"}
{"type":"leaf","id":"msg-1"}
"#;
    std::fs::write(&path, log).unwrap();
    let store = SessionStore::load(&path).unwrap();
    assert_eq!(store.leaf(), Some("msg-1"));
}

#[test]
fn session_log_tolerates_malformed_lines() {
    // Malformed lines are skipped; the leaf falls back to the last valid
    // message entry when the leaf record points at a nonexistent id.
    let dir = tempdir().unwrap();
    let path = dir.path().join("test.jsonl");
    let log = r#"{"type":"session","id":"s1","timestamp":"2024-01-01T00:00:00Z","cwd":"/tmp"}
not json at all
{}

{"type":"message","id":"msg-1","parent_id":null,"timestamp":"2024-01-01T00:00:00Z","role":"assistant","text":"hi"}
{"type":"leaf","id":"nonexistent"}
"#;
    std::fs::write(&path, log).unwrap();
    let store = SessionStore::load(&path).unwrap();
    // Leaf points at nonexistent id -> falls back to last message.
    assert_eq!(store.leaf(), Some("msg-1"));
}
