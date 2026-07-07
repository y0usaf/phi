//! phi-session — the JSONL session log (PLAN.md WS4.1).
//!
//! Append-only, pi-compatible in spirit: one `session` header line, then one
//! `message` entry per transcript [`Message`], stored under the phi data dir
//! keyed by cwd. The loop owns the transcript (`phi_ai_types::Context`); this
//! crate is the durable sink for it and the read path that reconstructs a
//! `Context` for `--continue`.
//!
//! Minimal by design: no compaction, no branch tree, no exchange tracking —
//! those are WS4.3 and later. The format is tagged (`Line`) so it grows
//! without breaking older readers (unknown lines are skipped on load).

mod store;
mod types;

pub use store::{
    flatten_cwd, list, list_in, load_messages, most_recent, most_recent_in, sessions_dir,
    sessions_dir_in, sessions_root,
};
pub use types::{Header, SESSION_VERSION, SessionMeta, SessionStore, new_id, now_timestamp};
