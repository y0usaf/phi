//! JSONL record shapes: one session header line, then one message entry per
//! transcript [`Message`]. Append-only; pi-compatible in spirit (header +
//! tagged entries, the tolerant reader skips malformed lines so a torn
//! final write from a crash doesn't lose the session).

use serde::{Deserialize, Serialize};

use phi_ai_types::Message;

/// Newest session-file format this build reads/writes. Files without a
/// version field (there are none yet, but the field is forward-compatible)
/// deserialize as 0 and load; files newer than this are rejected at load.
pub const SESSION_VERSION: u32 = 1;

/// First line of a session file. The `"type": "session"` discriminator is
/// added by [`Line`]'s serde tag, so `Header` carries only the payload.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Header {
    pub version: u32,
    /// UUID v7 — time-ordered, so lexicographic sort is creation order.
    pub id: String,
    /// RFC 3339 wall-clock creation time.
    pub timestamp: String,
    /// Working directory the session was recorded in.
    pub cwd: String,
    /// Model the session used (`provider/id`), for the resume listing and
    /// `/resume` picker. May diverge mid-session via `/model`; the header
    /// stays as the session's original model.
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session: Option<String>,
}

/// One transcript message, appended as its own JSONL line. The
/// `"type": "message"` discriminator is added by [`Line`]'s serde tag.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub message: Message,
}

/// One JSONL line; tagged so the format can grow (compaction, branch
/// navigation, …) without breaking older readers. Unknown variants are
/// skipped on load.
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Line {
    Session(Header),
    Message(Entry),
}

/// Listing metadata for the `/resume` picker and `--continue`'s
/// most-recent lookup.
#[derive(Clone, Debug)]
pub struct SessionMeta {
    /// Absolute path to the `.jsonl` file.
    pub path: std::path::PathBuf,
    pub id: String,
    pub model: String,
    pub cwd: String,
    /// First user prompt, for the picker label.
    pub first_message: String,
    /// Number of messages in the session.
    pub message_count: usize,
    /// RFC 3339 creation timestamp (from the header).
    pub created_at: String,
}

/// Per-session accounting: where to write and what's been written.
///
/// The store is an append-only sink: the loop owns the transcript
/// (`Context.messages`) and hands each finalized message to the store;
/// the store never holds the whole transcript in memory.
pub struct SessionStore {
    pub(crate) path: std::path::PathBuf,
    pub(crate) header: Header,
    /// Count of appended message entries (for `SessionMeta`).
    pub(crate) count: usize,
    /// First user prompt, captured for the resume label.
    pub(crate) first_message: Option<String>,
}

/// A new session id (UUID v7, time-ordered).
pub fn new_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// Current time as RFC 3339.
pub fn now_timestamp() -> String {
    jiff::Timestamp::now().to_string()
}
