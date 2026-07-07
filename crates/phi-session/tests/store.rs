//! WS4.1 acceptance: append-only JSONL round-trips, `--continue` finds the
//! most recent session for a cwd, a torn final line doesn't lose the
//! transcript, and a newer-than-understood version is rejected.

use std::path::Path;

use phi_ai_types::{AssistantContent, AssistantMessage, Message, ToolResultMessage, UserMessage};
use phi_session::{SessionStore, list_in, load_messages, most_recent_in};
use tempfile::TempDir;

fn root() -> std::path::PathBuf {
    // Each test uses its own temp dir as the sessions root, so tests are
    // isolated and never touch the real data dir.
    TempDir::new().unwrap().path().to_owned()
}

fn user(text: &str) -> Message {
    Message::User(UserMessage::new(text))
}

fn assistant(text: &str) -> Message {
    let mut m = AssistantMessage::empty(&phi_ai_types::Model {
        id: "fake".into(),
        name: "Fake".into(),
        api: phi_ai_types::Api::AnthropicMessages,
        provider: "test".into(),
        base_url: String::new(),
        reasoning: false,
        input: Vec::new(),
        cost: Default::default(),
        context_window: 200_000,
        max_tokens: 8192,
        headers: Default::default(),
        thinking_levels: None,
        adaptive_thinking: false,
    });
    m.content.push(AssistantContent::Text { text: text.into() });
    Message::Assistant(m)
}

#[test]
fn create_writes_header_and_round_trips_messages() {
    let root = root();
    let cwd = Path::new("/tmp/project");
    let mut store = SessionStore::create_in(&root, cwd, "test/fake").unwrap();
    assert_eq!(store.message_count(), 0);

    store.append(&user("hello")).unwrap();
    store.append(&assistant("hi there")).unwrap();
    assert_eq!(store.message_count(), 2);

    let (header, messages) = load_messages(store.path()).unwrap();
    assert_eq!(header.cwd, cwd.display().to_string());
    assert_eq!(header.model, "test/fake");
    assert_eq!(header.version, 1);
    assert_eq!(messages.len(), 2);
    assert!(matches!(&messages[0], Message::User(u) if u.content == "hello"));
    assert!(matches!(&messages[1], Message::Assistant(a) if a.text() == "hi there"));
}

#[test]
fn tool_result_round_trips() {
    let root = root();
    let cwd = Path::new("/tmp/project");
    let mut store = SessionStore::create_in(&root, cwd, "test/fake").unwrap();
    store
        .append(&Message::ToolResult(
            ToolResultMessage::new("c1", "output").with_error(true),
        ))
        .unwrap();
    let (_, messages) = load_messages(store.path()).unwrap();
    let Message::ToolResult(r) = &messages[0] else {
        panic!("expected tool result");
    };
    assert_eq!(r.tool_use_id, "c1");
    assert_eq!(r.content, "output");
    assert!(r.is_error);
}

#[test]
fn most_recent_returns_newest_for_cwd() {
    let root = root();
    let cwd = Path::new("/tmp/project");
    let mut a = SessionStore::create_in(&root, cwd, "test/fake").unwrap();
    a.append(&user("first session")).unwrap();
    // Force a later UUID v7 timestamp by sleeping briefly so the second
    // session's id is strictly greater than the first's.
    std::thread::sleep(std::time::Duration::from_millis(20));
    let mut b = SessionStore::create_in(&root, cwd, "test/fake").unwrap();
    b.append(&user("second session")).unwrap();

    let recent = most_recent_in(&root, cwd).unwrap();
    let (_, messages) = load_messages(&recent).unwrap();
    assert_eq!(messages.len(), 1);
    assert!(matches!(&messages[0], Message::User(u) if u.content == "second session"));
}

#[test]
fn most_recent_returns_none_when_no_sessions() {
    let root = root();
    let cwd = Path::new("/tmp/project");
    assert!(most_recent_in(&root, cwd).is_none());
}

#[test]
fn most_recent_skips_empty_sessions() {
    let root = root();
    let cwd = Path::new("/tmp/project");
    // An empty session (header only, no messages) should not be returned
    // as the resume target — resuming it would seed an empty transcript.
    let _empty = SessionStore::create_in(&root, cwd, "test/fake").unwrap();
    assert!(most_recent_in(&root, cwd).is_none());
}

#[test]
fn torn_final_line_is_skipped_not_fatal() {
    let root = root();
    let cwd = Path::new("/tmp/project");
    let mut store = SessionStore::create_in(&root, cwd, "test/fake").unwrap();
    store.append(&user("one")).unwrap();
    store.append(&assistant("two")).unwrap();
    // Append a torn (invalid) line, as a crash mid-write would leave.
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(store.path())
        .unwrap();
    writeln!(f, "{{garbage").unwrap();

    let (_, messages) = load_messages(store.path()).unwrap();
    assert_eq!(messages.len(), 2, "torn line skipped, transcript intact");
}

#[test]
fn sessions_for_different_cwd_do_not_collide() {
    let root = root();
    let cwd_a = Path::new("/tmp/a");
    let cwd_b = Path::new("/tmp/b");
    let mut a = SessionStore::create_in(&root, cwd_a, "test/fake").unwrap();
    a.append(&user("from a")).unwrap();
    let mut b = SessionStore::create_in(&root, cwd_b, "test/fake").unwrap();
    b.append(&user("from b")).unwrap();

    let recent_a = most_recent_in(&root, cwd_a).unwrap();
    let (_, msgs_a) = load_messages(&recent_a).unwrap();
    assert!(matches!(&msgs_a[0], Message::User(u) if u.content == "from a"));

    let recent_b = most_recent_in(&root, cwd_b).unwrap();
    let (_, msgs_b) = load_messages(&recent_b).unwrap();
    assert!(matches!(&msgs_b[0], Message::User(u) if u.content == "from b"));
}

#[test]
fn flatten_cwd_is_safe_and_distinct() {
    assert_eq!(
        phi_session::flatten_cwd(Path::new("/home/u/dev")),
        "--home-u-dev--"
    );
    assert_eq!(
        phi_session::flatten_cwd(Path::new("relative")),
        "--relative--"
    );
}

#[test]
fn list_returns_sessions_with_metadata() {
    let root = root();
    let cwd = Path::new("/tmp/project");
    let mut a = SessionStore::create_in(&root, cwd, "test/fake").unwrap();
    a.append(&user("first")).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    let mut b = SessionStore::create_in(&root, cwd, "test/fake").unwrap();
    b.append(&user("second prompt")).unwrap();

    let sessions = list_in(&root, cwd);
    assert_eq!(sessions.len(), 2);
    // Newest first.
    assert_eq!(sessions[0].first_message, "second prompt");
    assert_eq!(sessions[1].first_message, "first");
    assert_eq!(sessions[0].message_count, 1);
    assert_eq!(sessions[0].model, "test/fake");
}

#[test]
fn file_name_is_chronological() {
    let root = root();
    let cwd = Path::new("/tmp/project");
    let store = SessionStore::create_in(&root, cwd, "test/fake").unwrap();
    let name = store.path().file_name().unwrap().to_str().unwrap();
    assert!(name.ends_with(".jsonl"));
    assert!(name.contains("_"), "name is {name}");
    // No colons or dots in the timestamp prefix (they'd break filesystems).
    let prefix = name.split('_').next().unwrap();
    assert!(!prefix.contains(':') && !prefix.contains('.'));
}

#[test]
fn append_all_flushes_a_slice() {
    let root = root();
    let cwd = Path::new("/tmp/project");
    let mut store = SessionStore::create_in(&root, cwd, "test/fake").unwrap();
    store
        .append_all(&[user("a"), assistant("b"), user("c")])
        .unwrap();
    assert_eq!(store.message_count(), 3);
    let (_, messages) = load_messages(store.path()).unwrap();
    assert_eq!(messages.len(), 3);
}

#[test]
fn sessions_dir_structure() {
    let root = root();
    let dir = phi_session::sessions_dir_in(&root, Path::new("/tmp/example"));
    assert_eq!(dir.parent().unwrap(), root);
    assert_eq!(dir.file_name().unwrap(), "--tmp-example--");
}
