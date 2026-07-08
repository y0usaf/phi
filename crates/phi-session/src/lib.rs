//! phi-session — the JSONL session log (PLAN.md WS4.1).
//!
//! Append-only and tree-structured (pi-compatible): one `session` header
//! line, then one `message` entry per transcript [`Message`] carrying
//! `id`/`parentId`, stored under the phi data dir keyed by cwd. The loop
//! owns the transcript (`phi_ai_types::Context`); this crate is the durable
//! sink for it and the read path that reconstructs a `Context` for
//! `--continue` and a tree for the session-tree browser.
//!
//! Minimal by design: no compaction, no exchange tracking — those land
//! later. The format is tagged (`Line`) so it grows without breaking older
//! readers (unknown lines are skipped on load); v1 flat files migrate
//! lazily to the v2 tree shape on first branch/label/append.

mod store;
mod types;

pub use store::{
    ParsedTree, find_node, flatten_cwd, leaf_path, list, list_in, load_messages,
    messages_along_leaf_path, most_recent, most_recent_in, read_tree, sessions_dir,
    sessions_dir_in, sessions_root,
};
pub use types::{
    EntryFields, Header, LabelEntry, SESSION_VERSION, SessionMeta, SessionStore, TreeNode, new_id,
    now_timestamp,
};
