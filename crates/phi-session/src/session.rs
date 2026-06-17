//! Persistent sessions (port of pi's session-manager): append-only
//! JSONL files where message entries carry id/parent_id, forming a branch
//! tree. Files live under the phi data dir, in a per-cwd folder. Files are
//! created lazily on the first message, so empty sessions leave no trace.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, Eq, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Entry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
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
#[serde(rename_all = "snake_case")]
pub enum ReplKind {
    Agent,
    Shell,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ReplEntry {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub code: String,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
    pub duration_secs: f64,
    pub kind: ReplKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subcalls: Vec<SubcallRecord>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReplRecord {
    pub code: String,
    pub stdout: String,
    pub stderr: String,
    pub duration_secs: f64,
    pub kind: ReplKind,
    pub tools: Vec<ToolCall>,
    pub subcalls: Vec<SubcallRecord>,
}

/// A prelude tool invocation (bash, edit_file, ...) that ran inside a
/// REPL execution; persisted so resumed sessions re-render the same tool
/// cards the user saw live.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ToolCall {
    pub name: String,
    pub args: serde_json::Value,
    pub result: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub duration_secs: f64,
    /// RLM event depth where the tool ran. Defaults to root depth for logs
    /// written before nested activity was depth-preserving.
    #[serde(default)]
    pub depth: usize,
}

/// A sub-LLM call made inside a REPL execution (llm_query and friends).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SubcallRecord {
    pub model: String,
    pub prompt_preview: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub duration_secs: f64,
    /// RLM event depth where the subcall was observed. Defaults to root depth
    /// for older logs whose replay path rendered subcalls at depth 0.
    #[serde(default)]
    pub depth: usize,
}

/// Lifecycle status recorded on `exchange_end` lines.
#[derive(Serialize, Deserialize, Clone, Copy, Eq, PartialEq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum ExchangeStatus {
    Completed,
    Failed,
    Interrupted,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ExchangeStart {
    pub id: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ExchangeEnd {
    pub id: String,
    pub status: ExchangeStatus,
}

/// Durable branch-tip marker: tree navigation (set_leaf) is a state
/// transition, so it is logged like any other fact.
#[derive(Serialize, Deserialize, Clone)]
struct LeafRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id: Option<String>,
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
struct Header {
    id: String,
    timestamp: String,
    cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_session: Option<String>,
}

/// One JSONL line; pi-style tagged records so the format can grow.
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Line {
    Session(Header),
    Message(Entry),
    Repl(ReplEntry),
    Leaf(LeafRecord),
    ExchangeStart(ExchangeStart),
    ExchangeEnd(ExchangeEnd),
}

pub struct SessionStore {
    path: PathBuf,
    header: Header,
    entries: Vec<Entry>,
    repls: Vec<ReplEntry>,
    /// Tip of the active branch; new entries attach here.
    leaf: Option<String>,
    /// Whether the file (header) has been written yet.
    persisted: bool,
    /// Exchange currently in flight (between begin_exchange/end_exchange).
    open_exchange: Option<String>,
    /// Exchange found open at load time: a previous run died mid-turn.
    interrupted_exchange: Option<String>,
}

/// Listing metadata for the /resume picker.
pub struct SessionMeta {
    pub path: PathBuf,
    pub modified: SystemTime,
    pub message_count: usize,
    pub first_message: String,
}

fn now() -> String {
    jiff::Timestamp::now().to_string()
}

fn new_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// Per-cwd session directory, mirroring pi: `sessions/--{cwd with
/// separators flattened}--` under the phi data dir.
pub fn sessions_dir(cwd: &Path) -> Result<PathBuf> {
    Ok(sessions_root()?.join(flatten_cwd(cwd)))
}

/// Root of all per-cwd session directories: `{data dir}/sessions`.
pub fn sessions_root() -> Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "phi")
        .context("could not determine a home directory")?;
    Ok(dirs.data_dir().join("sessions"))
}

/// `--{cwd with separators flattened}--`, the per-cwd directory name.
pub fn flatten_cwd(cwd: &Path) -> String {
    format!(
        "--{}--",
        cwd.display()
            .to_string()
            .trim_start_matches(['/', '\\'])
            .replace(['/', '\\', ':'], "-")
    )
}

impl SessionStore {
    pub fn new(cwd: &Path) -> Result<Self> {
        let id = new_id();
        let path = sessions_dir(cwd)?.join(format!("{id}.jsonl"));
        Ok(Self {
            path,
            header: Header {
                id,
                timestamp: now(),
                cwd: cwd.display().to_string(),
                parent_session: None,
            },
            entries: Vec::new(),
            repls: Vec::new(),
            leaf: None,
            persisted: false,
            open_exchange: None,
            interrupted_exchange: None,
        })
    }

    pub fn load(path: &Path) -> Result<Self> {
        // Tolerant reader: a stray non-UTF8 byte must not lose the session,
        // so decode lossily instead of failing the whole load. Malformed
        // lines (including a torn final line from a crash mid-append) are
        // skipped below; truncation is the expected crash signature of an
        // append-only log, not corruption.
        let bytes = std::fs::read(path)
            .with_context(|| format!("failed to read session: {}", path.display()))?;
        let content = String::from_utf8_lossy(&bytes);
        let mut header = None;
        let mut entries = Vec::new();
        let mut repls = Vec::new();
        let mut leaf_record: Option<Option<String>> = None;
        let mut open_exchange: Option<String> = None;
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            // Skip malformed lines (pi-style) so one bad write doesn't lose
            // the whole session.
            match serde_json::from_str::<Line>(line) {
                Ok(Line::Session(h)) if header.is_none() => header = Some(h),
                Ok(Line::Message(entry)) => entries.push(entry),
                Ok(Line::Repl(repl)) => repls.push(repl),
                Ok(Line::Leaf(record)) => leaf_record = Some(record.id),
                Ok(Line::ExchangeStart(start)) => open_exchange = Some(start.id),
                Ok(Line::ExchangeEnd(end)) if open_exchange.as_deref() == Some(end.id.as_str()) => {
                    open_exchange = None;
                }
                _ => {}
            }
        }
        let header = header
            .with_context(|| format!("not a phi session file (no header): {}", path.display()))?;
        // The last persisted leaf record wins (durable tree navigation);
        // fall back to the newest entry for logs predating leaf records.
        // A recorded leaf pointing at an entry the tolerant reader dropped
        // falls back too, rather than orphaning the branch.
        let leaf = match leaf_record {
            Some(Some(id)) if entries.iter().any(|e| e.id == id) => Some(id),
            Some(None) => None,
            _ => entries.last().map(|e| e.id.clone()),
        };
        // An exchange_start without its exchange_end is the durable
        // signature of a crash mid-turn: surface it instead of hiding it.
        Ok(Self {
            path: path.to_owned(),
            header,
            entries,
            repls,
            leaf,
            persisted: true,
            open_exchange: None,
            interrupted_exchange: open_exchange,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Redirect the file path so tests never touch the real data dir.
    /// Test-only hook (used by phi-mux tests too).
    #[doc(hidden)]
    pub fn set_test_path(&mut self, path: PathBuf) {
        self.path = path;
    }

    pub fn id(&self) -> &str {
        &self.header.id
    }

    /// Working directory the session was recorded in (from the header).
    pub fn cwd(&self) -> &str {
        &self.header.cwd
    }

    pub fn leaf(&self) -> Option<&str> {
        self.leaf.as_deref()
    }

    pub fn entry(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// Move the active branch tip (tree navigation). `None` rewinds to the
    /// session root. Persisted as a leaf record so resume returns to the
    /// branch the user was actually on, not the newest entry.
    pub fn set_leaf(&mut self, id: Option<String>) {
        if self.persisted && id != self.leaf {
            // Best-effort durability: navigation must still work if the
            // disk write fails, so don't propagate the error.
            let _ = self.write_line(&Line::Leaf(LeafRecord { id: id.clone() }));
        }
        self.leaf = id;
    }

    /// Append a message on the active branch and persist it.
    pub fn append(&mut self, role: Role, text: &str) -> Result<String> {
        self.append_with_usage(role, text, None)
    }

    pub fn append_with_usage(
        &mut self,
        role: Role,
        text: &str,
        usage: Option<SessionUsage>,
    ) -> Result<String> {
        self.append_with_usage_and_display(role, text, None, usage)
    }

    pub fn append_with_usage_and_display(
        &mut self,
        role: Role,
        text: &str,
        display_text: Option<String>,
        usage: Option<SessionUsage>,
    ) -> Result<String> {
        let usage_by_model = usage.iter().cloned().collect();
        self.append_with_usage_breakdown_and_display(
            role,
            text,
            display_text,
            usage,
            usage_by_model,
        )
    }

    pub fn append_with_usage_breakdown_and_display(
        &mut self,
        role: Role,
        text: &str,
        display_text: Option<String>,
        usage: Option<SessionUsage>,
        usage_by_model: Vec<SessionUsage>,
    ) -> Result<String> {
        let entry = Entry {
            id: new_id(),
            parent_id: self.leaf.clone(),
            timestamp: now(),
            role,
            text: text.to_owned(),
            display_text,
            usage,
            usage_by_model,
        };
        // Disk first, memory second: an entry becomes a fact only once it
        // is durably logged, so a failed write must not leave in-memory
        // state ahead of the file.
        let id = self.write_line(&Line::Message(entry.clone()))?;
        self.leaf = Some(entry.id.clone());
        self.entries.push(entry);
        Ok(id)
    }

    pub fn append_repl(&mut self, record: ReplRecord) -> Result<String> {
        let entry = ReplEntry {
            id: new_id(),
            parent_id: self.leaf.clone(),
            timestamp: now(),
            code: record.code,
            stdout: record.stdout,
            stderr: record.stderr,
            duration_secs: record.duration_secs,
            kind: record.kind,
            tools: record.tools,
            subcalls: record.subcalls,
        };
        let id = self.write_line(&Line::Repl(entry.clone()))?;
        self.repls.push(entry);
        Ok(id)
    }

    /// Durably mark the start of an exchange (user turn). An
    /// `exchange_start` without a matching `exchange_end` is how a crash
    /// mid-turn stays visible after restart.
    pub fn begin_exchange(&mut self) -> Result<String> {
        let id = new_id();
        self.write_line(&Line::ExchangeStart(ExchangeStart { id: id.clone() }))?;
        self.open_exchange = Some(id.clone());
        Ok(id)
    }

    /// Durably close the open exchange with its terminal status.
    pub fn end_exchange(&mut self, status: ExchangeStatus) -> Result<()> {
        let Some(id) = self.open_exchange.take() else {
            return Ok(());
        };
        self.write_line(&Line::ExchangeEnd(ExchangeEnd { id, status }))?;
        Ok(())
    }

    /// Exchange left open by a previous run (crash/kill mid-turn), found at
    /// load time. Cleared by `acknowledge_interrupted`.
    pub fn interrupted_exchange(&self) -> Option<&str> {
        self.interrupted_exchange.as_deref()
    }

    /// Durably close out an exchange a previous run left open.
    pub fn acknowledge_interrupted(&mut self) -> Result<()> {
        if let Some(id) = self.interrupted_exchange.take() {
            self.write_line(&Line::ExchangeEnd(ExchangeEnd {
                id,
                status: ExchangeStatus::Interrupted,
            }))?;
        }
        Ok(())
    }

    fn write_line(&mut self, line: &Line) -> Result<String> {
        if !self.persisted {
            let dir = self.path.parent().expect("session path has a dir");
            std::fs::create_dir_all(dir)?;
            // The header write must be as durable as every append: sync the
            // file, then fsync the directory so the new name itself survives
            // a crash (a session that exists in memory but not on disk would
            // violate the log-is-the-session invariant).
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&self.path)?;
            writeln!(
                file,
                "{}",
                serde_json::to_string(&Line::Session(self.header.clone()))?
            )?;
            file.sync_data()?;
            if let Ok(dir_handle) = std::fs::File::open(dir) {
                let _ = dir_handle.sync_data();
            }
            self.persisted = true;
        }
        let mut file = std::fs::OpenOptions::new().append(true).open(&self.path)?;
        writeln!(file, "{}", serde_json::to_string(line)?)?;
        // The log is the session: a line is a fact only after it reaches
        // disk. Sessions append at human-interaction rates, so the fsync
        // cost is negligible next to the LLM/REPL work it protects.
        file.sync_data()?;
        Ok(match line {
            Line::Message(entry) => entry.id.clone(),
            Line::Session(header) => header.id.clone(),
            Line::Repl(entry) => entry.id.clone(),
            Line::Leaf(record) => record.id.clone().unwrap_or_default(),
            Line::ExchangeStart(start) => start.id.clone(),
            Line::ExchangeEnd(end) => end.id.clone(),
        })
    }

    /// Lineage of `id` (root → `id`), following parent links.
    fn lineage(&self, id: &str) -> Vec<&Entry> {
        let mut chain = Vec::new();
        let mut current = self.entry(id);
        while let Some(entry) = current {
            chain.push(entry);
            current = entry.parent_id.as_deref().and_then(|p| self.entry(p));
        }
        chain.reverse();
        chain
    }

    /// Entries on the active branch, root → leaf.
    pub fn branch(&self) -> Vec<&Entry> {
        self.leaf
            .as_deref()
            .map(|id| self.lineage(id))
            .unwrap_or_default()
    }

    /// Active-branch transcript items in JSONL/file order, interleaving
    /// assistant/user messages with the persisted REPL executions attached
    /// to the active branch lineage.
    pub fn branch_items(&self) -> Vec<BranchItem<'_>> {
        let branch = self.branch();
        if branch.is_empty() {
            return Vec::new();
        }
        let branch_ids: std::collections::HashSet<&str> =
            branch.iter().map(|e| e.id.as_str()).collect();
        let mut items = Vec::new();
        for entry in &self.entries {
            if branch_ids.contains(entry.id.as_str()) {
                items.push(BranchItem::Message(entry));
            }
            items.extend(
                self.repls
                    .iter()
                    .filter(|repl| repl.parent_id.as_deref() == Some(entry.id.as_str()))
                    .map(BranchItem::Repl),
            );
        }
        items
    }

    /// Active-branch turns reconstructed from the durable message/repl log.
    ///
    /// Agent REPL checkpoints may be parented either to the turn's user
    /// entry (current live checkpointing) or to its assistant entry (older
    /// tests/manual writes), so replay/restore must group both into one
    /// exchange instead of trusting flat parent order alone. Shell REPLs
    /// between turns become `pre_shell` on the following exchange; any
    /// trailing shell executions are returned separately.
    pub fn branch_exchanges(&self) -> (Vec<BranchExchange<'_>>, Vec<&ReplEntry>) {
        let branch = self.branch();
        if branch.is_empty() {
            return (Vec::new(), Vec::new());
        }
        let mut exchanges = Vec::new();
        let mut shell_buf = Vec::new();
        let mut open: Option<BranchExchange<'_>> = None;
        for entry in branch {
            let attached: Vec<&ReplEntry> = self
                .repls
                .iter()
                .filter(|repl| repl.parent_id.as_deref() == Some(entry.id.as_str()))
                .collect();
            match entry.role {
                Role::User => {
                    if let Some(exchange) = open.take() {
                        exchanges.push(exchange);
                    }
                    let mut exchange = BranchExchange {
                        prompt: entry,
                        response: None,
                        pre_shell: std::mem::take(&mut shell_buf),
                        repls: Vec::new(),
                    };
                    exchange.repls.extend(attached);
                    open = Some(exchange);
                }
                Role::Assistant => {
                    if let Some(mut exchange) = open.take() {
                        exchange.response = Some(entry);
                        for repl in attached {
                            match repl.kind {
                                ReplKind::Agent => exchange.repls.push(repl),
                                ReplKind::Shell => shell_buf.push(repl),
                            }
                        }
                        exchanges.push(exchange);
                    } else {
                        for repl in attached {
                            if repl.kind == ReplKind::Shell {
                                shell_buf.push(repl);
                            }
                        }
                    }
                }
            }
        }
        if let Some(exchange) = open.take() {
            exchanges.push(exchange);
        }
        (exchanges, shell_buf)
    }

    /// The active branch projected for model-state restore: pi-style, the
    /// canonical stream is resolved into typed exchanges carrying the REPL
    /// executions that ran inside each turn, plus any trailing user shell
    /// executions not yet replayed to the model.
    pub fn branch_restore(&self) -> (Vec<phi_rlm::RestoredExchange>, Vec<phi_rlm::RestoredRepl>) {
        fn restored(repl: &ReplEntry) -> phi_rlm::RestoredRepl {
            phi_rlm::RestoredRepl {
                code: repl.code.clone(),
                stdout: repl.stdout.clone(),
                stderr: repl.stderr.clone(),
            }
        }
        let (branch, trailing_shell) = self.branch_exchanges();
        let exchanges = branch
            .into_iter()
            .filter_map(|exchange| {
                let response = exchange.response?;
                Some(phi_rlm::RestoredExchange {
                    pre_shell: exchange.pre_shell.into_iter().map(restored).collect(),
                    prompt: exchange.prompt.text.clone(),
                    repls: exchange.repls.into_iter().map(restored).collect(),
                    response: response.text.clone(),
                })
            })
            .collect();
        let pending = trailing_shell
            .into_iter()
            .filter(|repl| repl.kind == ReplKind::Shell)
            .map(restored)
            .collect();
        (exchanges, pending)
    }

    /// All user messages in the session (any branch), chronological — the
    /// /fork candidates, mirroring pi's getUserMessagesForForking.
    pub fn user_messages(&self) -> Vec<&Entry> {
        self.entries
            .iter()
            .filter(|e| e.role == Role::User)
            .collect()
    }

    /// Depth-first walk of the whole entry tree as (depth, entry) rows, in
    /// file order within each sibling group — the /tree view.
    pub fn tree_rows(&self) -> Vec<(usize, &Entry)> {
        let mut rows = Vec::new();
        let roots: Vec<&Entry> = self
            .entries
            .iter()
            .filter(|e| e.parent_id.is_none())
            .collect();
        let mut stack: Vec<(usize, &Entry)> = roots.into_iter().rev().map(|e| (0, e)).collect();
        while let Some((depth, entry)) = stack.pop() {
            rows.push((depth, entry));
            let children: Vec<&Entry> = self
                .entries
                .iter()
                .filter(|e| e.parent_id.as_deref() == Some(&entry.id))
                .collect();
            stack.extend(children.into_iter().rev().map(|e| (depth + 1, e)));
        }
        rows
    }

    /// New session containing the lineage up to (but not including)
    /// `entry_id`, pi-style fork: the excluded entry's text goes back into
    /// the editor.
    pub fn fork(&self, entry_id: &str) -> Result<SessionStore> {
        let entry = self
            .entry(entry_id)
            .with_context(|| format!("unknown session entry: {entry_id}"))?;
        let kept: Vec<Entry> = match &entry.parent_id {
            Some(parent) => self.lineage(parent).into_iter().cloned().collect(),
            None => Vec::new(),
        };
        let id = new_id();
        let dir = self.path.parent().context("session path has a dir")?;
        let path = dir.join(format!("{id}.jsonl"));
        // Write the fork to a temp name and rename into place: a crash
        // mid-fork must not leave a half-copied session masquerading as a
        // real one (rename within a directory is atomic on POSIX).
        let tmp_path = dir.join(format!("{id}.jsonl.tmp"));
        let mut store = SessionStore {
            path: tmp_path.clone(),
            header: Header {
                id,
                timestamp: now(),
                cwd: self.header.cwd.clone(),
                parent_session: Some(self.path.display().to_string()),
            },
            entries: Vec::new(),
            repls: Vec::new(),
            leaf: kept.last().map(|e| e.id.clone()),
            persisted: false,
            open_exchange: None,
            interrupted_exchange: None,
        };
        for entry in kept {
            store.entries.push(entry.clone());
            store.write_line(&Line::Message(entry))?;
        }
        if store.persisted {
            std::fs::rename(&tmp_path, &path)?;
            if let Ok(dir_handle) = std::fs::File::open(dir) {
                let _ = dir_handle.sync_data();
            }
        }
        store.path = path;
        Ok(store)
    }

    /// Sessions in `cwd`'s session dir, most recently modified first.
    pub fn list(cwd: &Path) -> Vec<SessionMeta> {
        let Ok(dir) = sessions_dir(cwd) else {
            return Vec::new();
        };
        let Ok(read) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut sessions: Vec<SessionMeta> = read
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
            .filter_map(|e| {
                let path = e.path();
                let store = SessionStore::load(&path).ok()?;
                if store.entries.is_empty() {
                    return None;
                }
                let first_message = store
                    .entries
                    .iter()
                    .find(|e| e.role == Role::User)
                    .map(|e| e.text.clone())
                    .unwrap_or_default();
                Some(SessionMeta {
                    path,
                    modified: e.metadata().ok()?.modified().ok()?,
                    message_count: store.entries.len(),
                    first_message,
                })
            })
            .collect();
        sessions.sort_by_key(|s| std::cmp::Reverse(s.modified));
        sessions
    }
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

/// Coarse relative age for the /resume list ("now", "5m", "3h", "2d").
pub fn age(modified: SystemTime) -> String {
    let secs = modified.elapsed().map(|d| d.as_secs()).unwrap_or(0);
    match secs {
        0..60 => "now".to_owned(),
        60..3600 => format!("{}m", secs / 60),
        3600..86_400 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_in(dir: &Path) -> SessionStore {
        let mut store = SessionStore::new(dir).unwrap();
        // Redirect under the temp dir so tests never touch the real data dir.
        store.path = dir.join(format!("{}.jsonl", store.header.id));
        store
    }

    #[test]
    fn append_load_roundtrip_preserves_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = store_in(tmp.path());
        let u1 = store.append(Role::User, "first prompt").unwrap();
        store.append(Role::Assistant, "first answer").unwrap();
        store.append(Role::User, "second prompt").unwrap();
        store.append(Role::Assistant, "second answer").unwrap();

        let loaded = SessionStore::load(store.path()).unwrap();
        assert_eq!(loaded.entries.len(), 4);
        assert_eq!(loaded.branch().len(), 4);
        let (exchanges, pending) = loaded.branch_restore();
        assert!(pending.is_empty());
        assert_eq!(
            exchanges
                .iter()
                .map(|x| (x.prompt.as_str(), x.response.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("first prompt", "first answer"),
                ("second prompt", "second answer"),
            ]
        );
        assert_eq!(loaded.entry(&u1).unwrap().text, "first prompt");
    }

    #[test]
    fn set_leaf_branches_the_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = store_in(tmp.path());
        store.append(Role::User, "q1").unwrap();
        let a1 = store.append(Role::Assistant, "a1").unwrap();
        store.append(Role::User, "q2").unwrap();
        store.append(Role::Assistant, "a2").unwrap();

        // Rewind to a1 and take a different second turn.
        store.set_leaf(Some(a1.clone()));
        store.append(Role::User, "q2-alt").unwrap();
        store.append(Role::Assistant, "a2-alt").unwrap();

        let (exchanges, _) = store.branch_restore();
        assert_eq!(
            exchanges
                .iter()
                .map(|x| (x.prompt.as_str(), x.response.as_str()))
                .collect::<Vec<_>>(),
            vec![("q1", "a1"), ("q2-alt", "a2-alt")]
        );
        // The tree keeps both branches: a1 has two children.
        let rows = store.tree_rows();
        assert_eq!(rows.len(), 6);
        let a1_children = rows
            .iter()
            .filter(|(_, e)| e.parent_id.as_deref() == Some(a1.as_str()));
        assert_eq!(a1_children.count(), 2);
        // Unpaired user prompts are skipped in exchanges.
        store.set_leaf(store.user_messages().first().map(|e| e.id.clone()));
        assert!(store.branch_restore().0.is_empty());
    }

    #[test]
    fn fork_copies_lineage_before_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = store_in(tmp.path());
        store.append(Role::User, "q1").unwrap();
        store.append(Role::Assistant, "a1").unwrap();
        let q2 = store.append(Role::User, "q2").unwrap();
        store.append(Role::Assistant, "a2").unwrap();

        let fork = store.fork(&q2).unwrap();
        assert!(fork.path().starts_with(tmp.path()));
        let (exchanges, _) = fork.branch_restore();
        assert_eq!(
            exchanges
                .iter()
                .map(|x| (x.prompt.as_str(), x.response.as_str()))
                .collect::<Vec<_>>(),
            vec![("q1", "a1")]
        );
        // Forking from the first message yields an empty session.
        let first = store.user_messages()[0].id.clone();
        let empty = store.fork(&first).unwrap();
        assert!(empty.branch().is_empty());
    }

    #[test]
    fn repl_entries_roundtrip_and_follow_parent_order() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = store_in(tmp.path());
        store.append(Role::User, "q1").unwrap();
        let a1 = store.append(Role::Assistant, "a1").unwrap();
        store
            .append_repl(ReplRecord {
                code: "print(1)".to_owned(),
                stdout: "1
"
                .to_owned(),
                stderr: String::new(),
                duration_secs: 0.5,
                kind: ReplKind::Agent,
                tools: vec![ToolCall {
                    name: "bash".to_owned(),
                    args: serde_json::json!({"cmd": "ls"}),
                    result: "ok".to_owned(),
                    error: None,
                    duration_secs: 0.1,
                    depth: 0,
                }],
                subcalls: vec![SubcallRecord {
                    model: "haiku".to_owned(),
                    prompt_preview: "summarize".to_owned(),
                    result_preview: Some("done".to_owned()),
                    error: None,
                    duration_secs: 0.2,
                    depth: 0,
                }],
            })
            .unwrap();
        store.append(Role::User, "q2").unwrap();
        store.append(Role::Assistant, "a2").unwrap();

        let loaded = SessionStore::load(store.path()).unwrap();
        let items = loaded.branch_items();
        assert_eq!(items.len(), 5);
        match items[1] {
            BranchItem::Message(entry) => assert_eq!(entry.id, a1),
            _ => panic!("expected assistant message second"),
        }
        match items[2] {
            BranchItem::Repl(repl) => {
                assert_eq!(repl.code, "print(1)");
                assert_eq!(
                    repl.stdout,
                    "1
"
                );
                assert_eq!(repl.parent_id.as_deref(), Some(a1.as_str()));
                assert_eq!(repl.kind, ReplKind::Agent);
                assert_eq!(repl.tools.len(), 1);
                assert_eq!(repl.tools[0].name, "bash");
                assert_eq!(repl.tools[0].result, "ok");
                assert_eq!(repl.tools[0].depth, 0);
                assert_eq!(repl.subcalls.len(), 1);
                assert_eq!(repl.subcalls[0].model, "haiku");
                assert_eq!(repl.subcalls[0].depth, 0);
            }
            _ => panic!("expected repl after parent assistant"),
        }

        let (turns, trailing) = loaded.branch_exchanges();
        assert!(trailing.is_empty());
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].response.unwrap().id, a1);
        assert_eq!(turns[0].repls.len(), 1);
        assert_eq!(turns[0].repls[0].parent_id.as_deref(), Some(a1.as_str()));
    }

    #[test]
    fn old_repl_activity_records_default_to_root_depth() {
        let tool: ToolCall = serde_json::from_value(serde_json::json!({
            "name": "bash",
            "args": {"cmd": "echo hi"},
            "result": "hi\n",
            "error": null,
            "duration_secs": 0.1
        }))
        .unwrap();
        assert_eq!(tool.depth, 0);

        let subcall: SubcallRecord = serde_json::from_value(serde_json::json!({
            "model": "sub",
            "prompt_preview": "say hi",
            "result_preview": "hi",
            "duration_secs": 0.2
        }))
        .unwrap();
        assert_eq!(subcall.depth, 0);
    }

    #[test]
    fn repl_activity_depth_roundtrips() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = store_in(tmp.path());
        store.append(Role::User, "q1").unwrap();
        store
            .append_repl(ReplRecord {
                code: "print(llm_query('x'))".to_owned(),
                stdout: "done\n".to_owned(),
                stderr: String::new(),
                duration_secs: 0.5,
                kind: ReplKind::Agent,
                tools: vec![ToolCall {
                    name: "bash".to_owned(),
                    args: serde_json::json!({"cmd": "echo nested"}),
                    result: "nested\n".to_owned(),
                    error: None,
                    duration_secs: 0.1,
                    depth: 1,
                }],
                subcalls: vec![SubcallRecord {
                    model: "sub".to_owned(),
                    prompt_preview: "x".to_owned(),
                    result_preview: Some("y".to_owned()),
                    error: None,
                    duration_secs: 0.2,
                    depth: 1,
                }],
            })
            .unwrap();

        let loaded = SessionStore::load(store.path()).unwrap();
        let repl = loaded
            .branch_items()
            .into_iter()
            .find_map(|item| match item {
                BranchItem::Repl(repl) => Some(repl),
                BranchItem::Message(_) => None,
            })
            .expect("repl item");
        assert_eq!(repl.tools[0].depth, 1);
        assert_eq!(repl.subcalls[0].depth, 1);
    }

    #[test]
    fn user_parented_agent_repls_group_into_the_following_exchange() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = store_in(tmp.path());
        let q1 = store.append(Role::User, "q1").unwrap();
        store
            .append_repl(ReplRecord {
                code: "print(1)".to_owned(),
                stdout: "1
"
                .to_owned(),
                stderr: String::new(),
                duration_secs: 0.5,
                kind: ReplKind::Agent,
                tools: Vec::new(),
                subcalls: Vec::new(),
            })
            .unwrap();
        let a1 = store
            .append(Role::Assistant, "before\n```repl\nprint(1)\n```\nafter")
            .unwrap();

        let loaded = SessionStore::load(store.path()).unwrap();

        let items = loaded.branch_items();
        assert_eq!(items.len(), 3);
        match items[0] {
            BranchItem::Message(entry) => assert_eq!(entry.id, q1),
            _ => panic!("expected user message first"),
        }
        match items[1] {
            BranchItem::Repl(repl) => assert_eq!(repl.parent_id.as_deref(), Some(q1.as_str())),
            _ => panic!("expected user-parented repl second"),
        }
        match items[2] {
            BranchItem::Message(entry) => assert_eq!(entry.id, a1),
            _ => panic!("expected assistant message third"),
        }

        let (turns, trailing) = loaded.branch_exchanges();
        assert!(trailing.is_empty());
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].prompt.id, q1);
        assert_eq!(turns[0].response.unwrap().id, a1);
        assert_eq!(turns[0].repls.len(), 1);
        assert_eq!(turns[0].repls[0].code, "print(1)");
        assert_eq!(turns[0].repls[0].parent_id.as_deref(), Some(q1.as_str()));

        let (exchanges, pending) = loaded.branch_restore();
        assert!(pending.is_empty());
        assert_eq!(exchanges.len(), 1);
        assert_eq!(exchanges[0].prompt, "q1");
        assert_eq!(
            exchanges[0].response,
            "before\n```repl\nprint(1)\n```\nafter"
        );
        assert_eq!(exchanges[0].repls.len(), 1);
        assert_eq!(exchanges[0].repls[0].code, "print(1)");
    }

    #[test]
    fn assistant_display_text_roundtrips_but_restore_uses_semantic_text() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = store_in(tmp.path());
        let q1 = store.append(Role::User, "q1").unwrap();
        store
            .append_repl(ReplRecord {
                code: "answer['content'] = 'done'\nanswer['ready'] = True".to_owned(),
                stdout: String::new(),
                stderr: String::new(),
                duration_secs: 0.1,
                kind: ReplKind::Agent,
                tools: Vec::new(),
                subcalls: Vec::new(),
            })
            .unwrap();
        store
            .append_with_usage_and_display(
                Role::Assistant,
                "done",
                Some(
                    "Submitting.\n```repl\nanswer['content'] = 'done'\nanswer['ready'] = True\n```done"
                        .to_owned(),
                ),
                None,
            )
            .unwrap();

        let loaded = SessionStore::load(store.path()).unwrap();
        let (turns, trailing) = loaded.branch_exchanges();
        assert!(trailing.is_empty());
        assert_eq!(turns.len(), 1);
        let response = turns[0].response.unwrap();
        assert_eq!(response.text, "done");
        assert_eq!(
            response.display_text.as_deref(),
            Some(
                "Submitting.\n```repl\nanswer['content'] = 'done'\nanswer['ready'] = True\n```done"
            )
        );
        assert_eq!(turns[0].repls.len(), 1);
        assert_eq!(turns[0].repls[0].parent_id.as_deref(), Some(q1.as_str()));

        let (exchanges, pending) = loaded.branch_restore();
        assert!(pending.is_empty());
        assert_eq!(exchanges.len(), 1);
        assert_eq!(exchanges[0].prompt, "q1");
        assert_eq!(exchanges[0].response, "done");
    }

    #[test]
    fn open_exchange_is_detected_as_interrupted_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store_in(dir.path());
        store.append(Role::User, "q1").unwrap();
        store.begin_exchange().unwrap();
        // Crash here: no exchange_end. A reload must surface it.
        let mut loaded = SessionStore::load(store.path()).unwrap();
        assert!(loaded.interrupted_exchange().is_some());
        loaded.acknowledge_interrupted().unwrap();
        assert!(loaded.interrupted_exchange().is_none());
        // The acknowledgement is durable: a second load sees a closed log.
        let reloaded = SessionStore::load(store.path()).unwrap();
        assert!(reloaded.interrupted_exchange().is_none());
    }

    #[test]
    fn closed_exchange_is_not_interrupted_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store_in(dir.path());
        store.append(Role::User, "q1").unwrap();
        store.begin_exchange().unwrap();
        store.append(Role::Assistant, "a1").unwrap();
        store.end_exchange(ExchangeStatus::Completed).unwrap();
        let loaded = SessionStore::load(store.path()).unwrap();
        assert!(loaded.interrupted_exchange().is_none());
    }

    #[test]
    fn set_leaf_persists_across_reload() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store_in(dir.path());
        store.append(Role::User, "q1").unwrap();
        let a1 = store.append(Role::Assistant, "a1").unwrap();
        store.append(Role::User, "q2").unwrap();
        store.append(Role::Assistant, "a2").unwrap();

        // Rewind to a1: resume must return here, not to the newest entry.
        store.set_leaf(Some(a1.clone()));
        let loaded = SessionStore::load(store.path()).unwrap();
        assert_eq!(loaded.leaf(), Some(a1.as_str()));
        assert_eq!(loaded.branch().len(), 2);
    }

    #[test]
    fn load_survives_torn_tail_and_bad_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store_in(dir.path());
        store.append(Role::User, "q1").unwrap();
        store.append(Role::Assistant, "a1").unwrap();

        // Simulate a crash mid-append (torn final line) plus a line of
        // non-UTF8 garbage: both must be skipped, not fatal.
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(store.path())
            .unwrap();
        file.write_all(&[0xff, 0xfe, b'{', b'\n']).unwrap();
        file.write_all(b"{\"type\":\"message\",\"id\":\"tor")
            .unwrap();
        drop(file);

        let loaded = SessionStore::load(store.path()).unwrap();
        let branch = loaded.branch();
        assert_eq!(branch.len(), 2);
        assert_eq!(branch[1].text, "a1");
    }
}
