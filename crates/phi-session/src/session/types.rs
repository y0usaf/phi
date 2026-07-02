//! Persistent session types: append-only JSONL records and the in-memory
//! session store that reads/writes them. Types are shared across the
//! store, tree, reconstruction, and discovery submodules.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, Eq, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Entry {
    pub id: String,
    pub parent_id: Option<String>,
    /// RFC 3339 wall-clock time the entry was appended. Optional so
    /// pre-timestamp session files keep loading; usage reporting
    /// (usage/loading.rs) skips entries without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    pub role: Role,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<SessionUsage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub usage_by_model: Vec<SessionUsage>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Eq, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum ReplKind {
    Agent,
    Shell,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ReplEntry {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// RFC 3339 wall-clock time the repl block was appended. Optional so
    /// pre-timestamp session files keep loading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    /// Intermediate assistant prose that preceded this repl block during
    /// the live turn. Persisted so resumed sessions show the model's
    /// reasoning between steps (instead of collapsing all repls into one
    /// summary card) and so the restored model history carries the
    /// intermediate reasoning instead of empty strings.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prose: String,
    pub code: String,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
    #[serde(default)]
    pub duration_secs: f64,
    pub kind: ReplKind,
    #[serde(default)]
    pub tools: Vec<ToolCall>,
    #[serde(default)]
    pub subcalls: Vec<SubcallRecord>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ReplRecord {
    /// Intermediate assistant prose that preceded this repl block.
    /// Forwarded from `pending_prose` at checkpoint time so it lands in
    /// the durable JSONL and survives session resume.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prose: String,
    pub code: String,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
    #[serde(default)]
    pub duration_secs: f64,
    pub kind: ReplKind,
    #[serde(default)]
    pub tools: Vec<ToolCall>,
    #[serde(default)]
    pub subcalls: Vec<SubcallRecord>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ToolCall {
    pub name: String,
    pub args: serde_json::Value,
    pub result: String,
    /// Path to a tmp file holding the full output when `result` was
    /// truncated (pi-style spillover: bounded memory, unbounded tmp).
    /// `None` means `result` is complete. The path is ephemeral — a
    /// reload after restart may find it gone, in which case the
    /// truncated `result` tail is the usable fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_output_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub duration_secs: f64,
    #[serde(default)]
    pub depth: usize,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SubcallRecord {
    pub model: String,
    pub prompt_preview: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_preview: Option<String>,
    /// Full subagent response text, persisted for durable recovery on
    /// session restore. Not auto-injected into the model's context — the
    /// model must explicitly read the restored `subcalls_N` variable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub duration_secs: f64,
    #[serde(default)]
    pub depth: usize,
    /// Tokens/cost this subcall consumed (`model` inside mirrors the
    /// subcall's model). For `subagent` calls it covers the child's whole
    /// subtree, so deeper records in the same array are already included
    /// in their ancestors' usage — don't sum across depths. These records
    /// are attribution detail only: the turn's totals live on the
    /// assistant `Entry` and already include every subcall, so usage
    /// reporting must not count these a second time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<SessionUsage>,
}

/// Result of spilling large output to a tmp file (pi-style spillover).
/// The `tail` is what goes in the durable JSONL log; `full_output_path`
/// is the route back to the complete data.
#[derive(Clone, Debug, PartialEq)]
pub struct SpillResult {
    /// Truncated tail kept in the durable log.
    pub tail: String,
    /// Path to the tmp file holding the full output, or `None` if no
    /// spill was needed.
    pub full_output_path: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Eq, PartialEq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum ExchangeStatus {
    Completed,
    Interrupted,
    Failed,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ExchangeStart {
    pub id: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ExchangeEnd {
    pub id: String,
    pub status: ExchangeStatus,
}

#[derive(Serialize, Deserialize, Clone)]
pub(super) struct LeafRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) id: Option<String>,
}

/// Per-turn token/cost accounting recorded on assistant entries.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SessionUsage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cost_total: f64,
    pub model: String,
}

/// Newest session-file format this build reads/writes. Files without a
/// version field (pre-versioning) deserialize as 0 and load normally;
/// files newer than this are rejected at load instead of being
/// misparsed or clobbered by an older phi.
pub(super) const SESSION_VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Clone)]
pub(super) struct Header {
    pub(super) id: String,
    #[serde(default)]
    pub(super) version: u32,
    pub(super) cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) parent_session: Option<String>,
}

/// One JSONL line; pi-style tagged records so the format can grow.
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum Line {
    Session(Header),
    Message(Entry),
    Repl(ReplEntry),
    Leaf(LeafRecord),
    ExchangeStart(ExchangeStart),
    ExchangeEnd(ExchangeEnd),
}

/// One interactive phi session: an append-only JSONL file whose entries
/// carry id/parent_id forming a branch tree, plus in-memory child indices.
pub struct SessionStore {
    pub(super) path: PathBuf,
    pub(super) header: Header,
    pub(super) entries: Vec<Entry>,
    pub(super) repls: Vec<ReplEntry>,
    /// Tip of the active branch; new entries attach here.
    pub(super) leaf: Option<String>,
    /// Whether the file (header) has been written yet.
    pub(super) persisted: bool,
    /// Exchange currently in flight (between begin_exchange/end_exchange).
    pub(super) open_exchange: Option<String>,
    /// Exchange found open at load time: a previous run died mid-turn.
    pub(super) interrupted_exchange: Option<String>,
    /// Child-entry index: parent_id → sorted list of child entry ids.
    /// Maintained on insert/load to avoid O(N) linear scans in tree
    /// traversals (tree_rows, branch_items, branch_exchanges).
    pub(super) entry_children: HashMap<String, Vec<String>>,
    /// Child-repl index: parent_id → ordered list of child repl ids.
    /// Same purpose as `entry_children` for repl lookups.
    pub(super) repl_children: HashMap<String, Vec<String>>,
}

/// Listing metadata for the /resume picker.
pub struct SessionMeta {
    pub path: PathBuf,
    pub modified: std::time::SystemTime,
    /// Session ID (a UUID v7); used for stable creation-order sorting so
    /// the sidebar doesn't reshuffle on every file write (turn/message).
    pub id: String,
    pub message_count: usize,
    pub first_message: String,
}

pub enum BranchItem<'a> {
    Message(&'a Entry),
    Repl(&'a ReplEntry),
}

pub struct BranchExchange<'a> {
    pub prompt: &'a Entry,
    pub response: Option<&'a Entry>,
    pub pre_shell: Vec<&'a ReplEntry>,
    pub repls: Vec<&'a ReplEntry>,
}

pub(super) fn new_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

pub(super) fn now_timestamp() -> String {
    jiff::Timestamp::now().to_string()
}
