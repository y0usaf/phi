//! JSONL record shapes: one session header line, then one tagged entry per
//! transcript turn. Append-only and tree-structured (pi-compatible): every
//! entry carries `id`/`parentId` so a session is a tree, not a flat list,
//! and a `leafId` pointer tracks the current position. Branching moves the
//! leaf to an earlier entry so new turns form a new branch without mutating
//! history — the foundation the session-tree browser navigates.
//!
//! The tolerant reader skips malformed lines (a torn final write from a
//! crash doesn't lose the session) and accepts v1 flat files (entries
//! without `id`/`parentId`) by synthesizing a linear chain, then rewriting
//! them to v2 on the next branch so the file upgrades lazily.

use serde::{Deserialize, Serialize};

use phi_ai_types::Message;

/// Newest session-file format this build reads/writes.
///
/// - v1: flat — entries are bare `message` lines with no `id`/`parentId`.
/// - v2: tree — entries carry `id`/`parentId`, plus `label`/`leaf` tags; a
///   session is a tree pointed at by `leafId`, not a flat list.
///
/// Files with no version (none yet exist) load as 0 and migrate; files
/// newer than this are rejected at load.
pub const SESSION_VERSION: u32 = 2;

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
    /// The id of the active leaf entry, or `None` when the session points
    /// at "before the first entry" (a fresh session, or a reset to re-edit
    /// the first user message). v1 files have none and synthesize it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leaf_id: Option<String>,
}

/// One transcript message in the tree. The `"type": "message"` and
/// `id`/`parentId` fields are added by [`Line`]'s serde tag + the shared
/// [`EntryFields`]; this struct carries only the message payload.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MessageEntry {
    pub message: Message,
}

/// Fields every tree entry (every line except the session header) carries:
/// `id` (this entry, a short collision-checked id), `parentId` (the entry
/// it branches from, or `None` for a root), and `timestamp`. Serializing
/// via `#[serde(flatten)]` keeps them on the same JSON object as the
/// variant payload, exactly like pi's `SessionEntryBase`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EntryFields {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    pub timestamp: String,
}

/// A user-defined label on a target entry. `label: None` clears it.
/// Appended as its own line (so labels are cheap to scan) and resolved
/// into [`SessionStore`]'s label map on load.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LabelEntry {
    #[serde(flatten)]
    pub fields: EntryFields,
    /// The entry this label attaches to (need not be the leaf).
    pub target_id: String,
    pub label: Option<String>,
}

/// One JSONL line. Tagged so the format grows (compaction summaries, model
/// changes, custom entries, …) without breaking older readers; unknown
/// variants are skipped on load. The message and tag variants share the
/// flattened `id`/`parentId`/`timestamp` so a tree view reads one shape.
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Line {
    Session(Header),
    Message {
        #[serde(flatten)]
        fields: EntryFields,
        #[serde(flatten)]
        message: MessageEntry,
    },
    Label {
        #[serde(flatten)]
        fields: EntryFields,
        target_id: String,
        label: Option<String>,
    },
}

/// A tree node: an entry plus its resolved child nodes (oldest first). The
/// read path ([`SessionStore::tree`]) returns this so the browser never
/// touches the flat line list.
#[derive(Clone, Debug)]
pub struct TreeNode {
    pub fields: EntryFields,
    pub message: Message,
    /// User-defined label resolved from the latest `label` entry, if any.
    pub label: Option<String>,
    pub label_timestamp: Option<String>,
    pub children: Vec<TreeNode>,
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

impl TreeNode {
    /// This entry's id (the tree key).
    pub fn id(&self) -> &str {
        &self.fields.id
    }

    /// The id of the entry this one branches from (`None` for a root).
    pub fn parent_id(&self) -> Option<&str> {
        self.fields.parent_id.as_deref()
    }
}

/// A new session id (UUID v7, time-ordered).
pub fn new_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// A short (8-hex-char) id unique within a session's existing ids.
/// Collision-checked against the passed set; falls back to a full UUID v4
/// after 100 attempts (pi's stance — never blocks the loop on an id).
pub fn short_id(existing: &std::collections::HashSet<String>) -> String {
    for _ in 0..100 {
        let id = uuid::Uuid::new_v4().simple().to_string()[..8].to_owned();
        if !existing.contains(&id) {
            return id;
        }
    }
    uuid::Uuid::new_v4().to_string()
}

/// Current time as RFC 3339.
pub fn now_timestamp() -> String {
    jiff::Timestamp::now().to_string()
}
