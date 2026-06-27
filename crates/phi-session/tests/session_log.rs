//! Tests for session log recovery.

use phi_session::session::SessionStore;
use tempfile::tempdir;

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
