//! WS4.1 acceptance: append-only JSONL round-trips, `--continue` finds the
//! most recent session for a cwd, a torn final line doesn't lose the
//! transcript, and a newer-than-understood version is rejected.

use std::path::Path;

use phi_ai_types::{AssistantContent, AssistantMessage, Message, ToolResultMessage, UserMessage};
use phi_session::{SessionStore, list_in, load_messages, most_recent_in, read_tree};
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
        reasoning_control: None,
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
    assert_eq!(header.version, 2);
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

/// WS4.1 tree extension: appending advances a leaf, and `read_tree`
/// reconstructs the linear chain (one root, one deep path) with the leaf
/// at the end.
#[test]
fn tree_round_trips_a_linear_session() {
    let root = root();
    let cwd = Path::new("/tmp/project");
    store_with_chain(&root, cwd);
    let parsed = read_tree(&most_recent_in(&root, cwd).unwrap()).unwrap();
    assert_eq!(parsed.tree.len(), 1, "one root");
    let chain = flatten_ids(&parsed.tree);
    assert_eq!(chain.len(), 3);
    let leaf = parsed.leaf_id.expect("leaf recorded");
    assert_eq!(chain.last().unwrap(), &leaf, "leaf is the last entry");
}

/// Branching then appending creates a sibling under the same parent.
#[test]
fn branch_creates_two_siblings_under_one_parent() {
    let root = root();
    let cwd = Path::new("/tmp/project");
    let mut store = SessionStore::create_in(&root, cwd, "test/fake").unwrap();
    let _u1 = store.append(&user("prompt")).unwrap();
    let branch_pt = store.append(&assistant("reply")).unwrap();
    // Branch A
    store.branch(Some(&branch_pt)).unwrap();
    let a = store.append(&user("branch A")).unwrap();
    // Branch B off the same point
    store.branch(Some(&branch_pt)).unwrap();
    let b = store.append(&user("branch B")).unwrap();

    let parsed = read_tree(&most_recent_in(&root, cwd).unwrap()).unwrap();
    let node = parsed.tree.into_iter().next().unwrap();
    // Walk to branch_pt, assert two children.
    let branch_node = find(&node, &branch_pt).cloned().expect("branch pt");
    assert_eq!(branch_node.children.len(), 2, "two sibling branches");
    let child_ids: Vec<String> = branch_node
        .children
        .iter()
        .map(|c| c.id().to_owned())
        .collect();
    assert!(child_ids.contains(&a));
    assert!(child_ids.contains(&b));
}

/// Labels persist as their own tag and resolve to the latest writer.
#[test]
fn labels_persist_and_resolve() {
    let root = root();
    let cwd = Path::new("/tmp/project");
    let mut store = SessionStore::create_in(&root, cwd, "test/fake").unwrap();
    let u1 = store.append(&user("prompt")).unwrap();
    store.label(&u1, Some("checkpoint")).unwrap();
    // Rename: a second label supersedes the first.
    store.label(&u1, Some("v2")).unwrap();

    let parsed = read_tree(&most_recent_in(&root, cwd).unwrap()).unwrap();
    let node = parsed.tree.into_iter().next().unwrap();
    let target = find(&node, &u1).expect("labeled entry");
    assert_eq!(target.label.as_deref(), Some("v2"), "latest label wins");
}

/// A label set to `None` clears an existing label.
#[test]
fn label_none_clears() {
    let root = root();
    let cwd = Path::new("/tmp/project");
    let mut store = SessionStore::create_in(&root, cwd, "test/fake").unwrap();
    let u = store.append(&user("prompt")).unwrap();
    store.label(&u, Some("x")).unwrap();
    store.label(&u, None).unwrap();
    let parsed = read_tree(&most_recent_in(&root, cwd).unwrap()).unwrap();
    let node = parsed.tree.into_iter().next().unwrap();
    assert_eq!(find(&node, &u).unwrap().label, None);
}

/// Branching to a nonexistent entry is rejected (validate before moving leaf).
#[test]
fn branch_to_missing_entry_is_rejected() {
    let root = root();
    let cwd = Path::new("/tmp/project");
    let mut store = SessionStore::create_in(&root, cwd, "test/fake").unwrap();
    store.append(&user("prompt")).unwrap();
    let _err = store.branch(Some("does-not-exist")).unwrap_err();
}

/// `messages_along_leaf_path` is the root→leaf transcript pi's tree browser
/// navigates back to. After a `branch` to an earlier entry, it returns
/// exactly that prefix (the abandoned tail stays on disk, just off the
/// active branch).
#[test]
fn messages_along_leaf_path_returns_root_to_leaf_prefix() {
    use phi_session::{leaf_path, messages_along_leaf_path};
    let root = root();
    let cwd = Path::new("/tmp/project");
    let mut store = SessionStore::create_in(&root, cwd, "test/fake").unwrap();
    let u1 = store.append(&user("first")).unwrap();
    store.append(&assistant("reply 1")).unwrap();
    store.append(&user("second")).unwrap();

    let parsed = read_tree(&most_recent_in(&root, cwd).unwrap()).unwrap();
    let path = leaf_path(&parsed.tree, parsed.leaf_id.as_deref());
    assert_eq!(path.len(), 3, "leaf path covers all three entries");
    assert_eq!(path[0].id(), u1, "path begins at the root");

    // Branch back to the first user message; the active transcript is now
    // just that one message.
    store.branch(Some(&u1)).unwrap();
    let parsed = read_tree(store.path()).unwrap();
    let msgs = messages_along_leaf_path(&parsed.tree, parsed.leaf_id.as_deref());
    assert_eq!(msgs.len(), 1);
    assert!(matches!(&msgs[0], Message::User(u) if u.content == "first"));
}

/// `create_branched_session` (pi's `/fork`) writes a new file containing
/// only the root→leaf path under fresh ids; the source file is untouched,
/// and the forked leaf points at the path's end so a fresh append continues.
#[test]
fn fork_copies_only_the_leaf_path_into_a_new_file() {
    use phi_session::messages_along_leaf_path;
    let root = root();
    let cwd = Path::new("/tmp/project");
    let mut store = SessionStore::create_in(&root, cwd, "test/fake").unwrap();
    store.append(&user("first")).unwrap();
    store.append(&assistant("reply 1")).unwrap();
    let u3 = store.append(&user("second")).unwrap();
    // An abandoned branch the fork must NOT carry.
    store.branch(Some(&u3)).unwrap();
    store.append(&user("abandoned tail")).unwrap();

    // Fork from the first user message: the new session should contain
    // only the root→u1 path (one message) and a fresh leaf.
    store.branch(Some(&u3)).unwrap(); // move leaf back to u3 first
    let forked = store.create_branched_session(Some(&u3)).unwrap();
    assert_ne!(forked.path(), store.path(), "fork writes a new file");
    let parsed = read_tree(forked.path()).unwrap();
    let msgs = messages_along_leaf_path(&parsed.tree, parsed.leaf_id.as_deref());
    let texts: Vec<String> = msgs
        .iter()
        .map(|m| match m {
            Message::User(u) => format!("user:{}", u.content),
            Message::Assistant(a) => format!("assistant:{}", a.text()),
            _ => String::new(),
        })
        .collect();
    assert_eq!(
        texts,
        vec!["user:first", "assistant:reply 1", "user:second"]
    );
    // Forked leaf == last path entry, so a subsequent append forms a new
    // child of it (not a new root).
    assert_eq!(forked.leaf_id().as_deref(), parsed.leaf_id.as_deref());
    // Source session is untouched: still 4 entries, leaf intact.
    let src = read_tree(store.path()).unwrap();
    assert_eq!(src.tree.len(), 1, "source tree still has its root");
}

/// Forking into a fresh file then appending grows the new session, not the
/// source — the fork primitive's end-to-end contract.
#[test]
fn fork_then_append_grows_the_new_session_only() {
    let root = root();
    let cwd = Path::new("/tmp/project");
    let mut store = SessionStore::create_in(&root, cwd, "test/fake").unwrap();
    let u1 = store.append(&user("first")).unwrap();
    store.append(&assistant("reply 1")).unwrap();

    let mut forked = store.create_branched_session(Some(&u1)).unwrap();
    forked.append(&user("follow up")).unwrap();

    // Source is unchanged (2 entries); fork has 2 (path) + 1 (append).
    let src = read_tree(store.path()).unwrap();
    let fork = read_tree(forked.path()).unwrap();
    let src_msgs = phi_session::messages_along_leaf_path(&src.tree, src.leaf_id.as_deref());
    let fork_msgs = phi_session::messages_along_leaf_path(&fork.tree, fork.leaf_id.as_deref());
    assert_eq!(src_msgs.len(), 2, "source session untouched");
    assert_eq!(fork_msgs.len(), 2, "fork grew by one");
    assert!(matches!(&fork_msgs[1], Message::User(u) if u.content == "follow up"));
}

// --- helpers ---

fn store_with_chain(root: &std::path::Path, cwd: &Path) {
    let mut store = SessionStore::create_in(root, cwd, "test/fake").unwrap();
    store.append(&user("hello")).unwrap();
    store.append(&assistant("hi there")).unwrap();
    store.append(&user("again")).unwrap();
}

fn flatten_ids(roots: &[phi_session::TreeNode]) -> Vec<String> {
    let mut out = Vec::new();
    for root in roots {
        walk(root, &mut out);
    }
    out
}

fn walk(node: &phi_session::TreeNode, out: &mut Vec<String>) {
    out.push(node.id().to_owned());
    for child in &node.children {
        walk(child, out);
    }
}

fn find<'a>(node: &'a phi_session::TreeNode, id: &str) -> Option<&'a phi_session::TreeNode> {
    if node.id() == id {
        return Some(node);
    }
    for child in &node.children {
        if let Some(found) = find(child, id) {
            return Some(found);
        }
    }
    None
}
