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

#[derive(Serialize, Deserialize, Clone)]
pub(super) struct Header {
    pub(super) id: String,
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
