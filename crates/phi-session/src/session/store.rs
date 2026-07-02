//! Session persistence: file I/O, append-only writes, exchange lifecycle,
//! and path helpers.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

use super::types::{
    CompactionEntry, ExchangeEnd, ExchangeStart, ExchangeStatus, Header, LeafRecord, Line,
    ReplEntry, ReplRecord, Role, SESSION_VERSION, SessionStore, SpillResult, new_id, now_timestamp,
};

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
                version: SESSION_VERSION,
                cwd: cwd.display().to_string(),
                parent_session: None,
            },
            entries: Vec::new(),
            repls: Vec::new(),
            compactions: Vec::new(),
            leaf: None,
            persisted: false,
            open_exchange: None,
            interrupted_exchange: None,
            entry_children: HashMap::new(),
            repl_children: HashMap::new(),
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
        let mut compactions = Vec::new();
        // Effective leaf, determined pi-style: the last durable line in
        // file order wins. A Message entry sets the leaf to its own id
        // (every appended entry becomes the branch tip); a Leaf record
        // (from tree navigation) sets it to the recorded target. This
        // ensures that messages appended after a `set_leaf` navigation
        // correctly override the stale leaf on reload.
        let mut leaf: Option<String> = None;
        let mut open_exchange: Option<String> = None;
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            // Skip malformed lines (pi-style) so one bad write doesn't lose
            // the whole session. Stream-deserialize each line: a historical
            // multi-process append race glued two records onto one line
            // (`{..}{..}`), and both must still load; parsing stops at the
            // first malformed record on the line.
            let mut records = serde_json::Deserializer::from_str(line).into_iter::<Line>();
            while let Some(Ok(record)) = records.next() {
                match record {
                    Line::Session(h) if header.is_none() => header = Some(h),
                    Line::Message(entry) => {
                        leaf = Some(entry.id.clone());
                        entries.push(entry);
                    }
                    Line::Repl(repl) => repls.push(repl),
                    Line::Compaction(entry) => compactions.push(entry),
                    Line::Leaf(record) => leaf = record.id,
                    Line::ExchangeStart(start) => open_exchange = Some(start.id),
                    Line::ExchangeEnd(end) if open_exchange.as_deref() == Some(end.id.as_str()) => {
                        open_exchange = None;
                    }
                    _ => {}
                }
            }
        }
        let header = header
            .with_context(|| format!("not a phi session file (no header): {}", path.display()))?;
        // A file from a newer phi may carry records this build would
        // misread or silently drop on rewrite — refuse instead.
        anyhow::ensure!(
            header.version <= SESSION_VERSION,
            "session file version {} is newer than this phi understands (max {}): {}",
            header.version,
            SESSION_VERSION,
            path.display()
        );
        // Validate the leaf: a leaf pointing at an entry the tolerant
        // reader dropped (malformed line) falls back to the newest entry
        // rather than orphaning the branch. A None leaf (navigation to
        // root) is preserved.
        let leaf = match leaf {
            Some(id) if entries.iter().any(|e| e.id == id) => Some(id),
            Some(_) => entries.last().map(|e| e.id.clone()),
            None => None,
        };
        // An exchange_start without its exchange_end is the durable
        // signature of a crash mid-turn: surface it instead of hiding it.
        let mut entry_children: HashMap<String, Vec<String>> = HashMap::new();
        for e in &entries {
            if let Some(pid) = &e.parent_id {
                entry_children
                    .entry(pid.clone())
                    .or_default()
                    .push(e.id.clone());
            }
        }
        let mut repl_children: HashMap<String, Vec<String>> = HashMap::new();
        for r in &repls {
            if let Some(pid) = &r.parent_id {
                repl_children
                    .entry(pid.clone())
                    .or_default()
                    .push(r.id.clone());
            }
        }
        Ok(Self {
            path: path.to_owned(),
            header,
            entries,
            repls,
            compactions,
            leaf,
            persisted: true,
            open_exchange: None,
            interrupted_exchange: open_exchange,
            entry_children,
            repl_children,
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

    /// Move the active branch tip (tree navigation). `None` rewinds to the
    /// session root. Persisted as a leaf record so resume returns to the
    /// branch the user was actually on, not the newest entry.
    pub fn set_leaf(&mut self, id: Option<String>) -> Result<()> {
        if self.persisted && id != self.leaf {
            // Propagate the write error: if the disk write fails, the
            // in-memory tree must NOT be updated, otherwise memory and
            // disk diverge and a crash would silently resume on the
            // wrong branch.
            self.write_line(&Line::Leaf(LeafRecord { id: id.clone() }))?;
        }
        self.leaf = id;
        Ok(())
    }

    /// Append a message on the active branch and persist it.
    pub fn append(&mut self, role: Role, text: &str) -> Result<String> {
        self.append_entry(role, text, None, None, Vec::new())
    }

    /// Append a message on the active branch and persist it, with optional
    /// display text, usage, and per-model usage breakdown.
    pub fn append_entry(
        &mut self,
        role: Role,
        text: &str,
        display_text: Option<String>,
        usage: Option<super::types::SessionUsage>,
        usage_by_model: Vec<super::types::SessionUsage>,
    ) -> Result<String> {
        let entry = super::types::Entry {
            id: new_id(),
            parent_id: self.leaf.clone(),
            timestamp: Some(now_timestamp()),
            role,
            text: text.to_owned(),
            display_text,
            usage,
            usage_by_model,
        };
        // Disk first, memory second: an entry becomes a fact only once it
        // is logged, so a failed write must not leave in-memory state
        // ahead of the file.
        let id = self.write_line(&Line::Message(entry.clone()))?;
        self.leaf = Some(entry.id.clone());
        if let Some(pid) = &entry.parent_id {
            self.entry_children
                .entry(pid.clone())
                .or_default()
                .push(entry.id.clone());
        }
        self.entries.push(entry);
        Ok(id)
    }

    pub fn append_repl(&mut self, record: ReplRecord) -> Result<String> {
        let entry = ReplEntry {
            id: new_id(),
            parent_id: self.leaf.clone(),
            timestamp: Some(now_timestamp()),
            prose: record.prose,
            code: record.code,
            stdout: record.stdout,
            stderr: record.stderr,
            duration_secs: record.duration_secs,
            kind: record.kind,
            tools: record.tools,
            subcalls: record.subcalls,
        };
        let id = self.write_line(&Line::Repl(entry.clone()))?;
        if let Some(pid) = &entry.parent_id {
            self.repl_children
                .entry(pid.clone())
                .or_default()
                .push(entry.id.clone());
        }
        self.repls.push(entry);
        Ok(id)
    }

    /// Append an in-loop compaction marker on the active branch. Additive
    /// only: message/repl records are never rewritten — the marker records
    /// that the engine folded live history into `summary` mid-turn, and
    /// reconstruction splices the summary into replayed context.
    pub fn append_compaction(
        &mut self,
        iteration: usize,
        tokens_before: u64,
        summary: &str,
    ) -> Result<String> {
        let entry = CompactionEntry {
            id: new_id(),
            parent_id: self.leaf.clone(),
            timestamp: Some(now_timestamp()),
            iteration,
            tokens_before,
            summary: summary.to_owned(),
        };
        let id = self.write_line(&Line::Compaction(entry.clone()))?;
        self.compactions.push(entry);
        Ok(id)
    }

    /// Compaction markers attached to `parent_id`, in append order.
    pub fn child_compactions(&self, parent_id: &str) -> Vec<&CompactionEntry> {
        self.compactions
            .iter()
            .filter(|c| c.parent_id.as_deref() == Some(parent_id))
            .collect()
    }

    /// Spill large tool output to a tmp file (pi-style spillover).
    ///
    /// When a tool result exceeds `max_bytes`, write the full output to a
    /// temp file and return a truncated tail plus the path. The caller
    /// stores the tail in the durable JSONL entry and the path in
    /// `ToolCall::full_output_path`, keeping the log lean while preserving
    /// a route back to the full data. The temp file is ephemeral: if it's
    /// gone after a restart, the truncated tail is the usable fallback.
    ///
    /// If the output fits within `max_bytes`, no spill occurs and the
    /// full output is returned as-is with no path.
    pub fn spill_output(output: &str, max_bytes: usize) -> Result<SpillResult> {
        if output.len() <= max_bytes {
            return Ok(SpillResult {
                tail: output.to_owned(),
                full_output_path: None,
            });
        }
        let mut tmp = tempfile::Builder::new()
            .prefix("phi-bash-")
            .suffix(".log")
            .tempfile()?;
        std::io::Write::write_all(&mut tmp, output.as_bytes())?;
        std::io::Write::flush(&mut tmp)?;
        let path = tmp.into_temp_path();
        let path_str = path.display().to_string();
        path.keep()?;
        let mut start = output.len().saturating_sub(max_bytes);
        // Round down to the nearest UTF-8 char boundary so we never slice
        // into the middle of a multi-byte character (which would panic).
        while start < output.len() && !output.is_char_boundary(start) {
            start += 1;
        }
        let tail = output[start..].to_owned();
        Ok(SpillResult {
            tail,
            full_output_path: Some(path_str),
        })
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

    pub(super) fn write_line(&mut self, line: &Line) -> Result<String> {
        if !self.persisted {
            let dir = self.path.parent().expect("session path has a dir");
            std::fs::create_dir_all(dir)?;
            // The header write must be fully durable: sync the file, then
            // fsync the directory so the new name itself survives a crash
            // (a session that exists in memory but not on disk would
            // violate the log-is-the-session invariant).
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&self.path)?;
            let mut header_buf = serde_json::to_string(&Line::Session(self.header.clone()))?;
            header_buf.push('\n');
            file.write_all(header_buf.as_bytes())?;
            file.sync_data()?;
            if let Ok(dir_handle) = std::fs::File::open(dir) {
                let _ = dir_handle.sync_data();
            }
            self.persisted = true;
        }
        let mut file = std::fs::OpenOptions::new().append(true).open(&self.path)?;
        // One buffer, one write: `writeln!` issues separate syscalls for
        // the JSON and the trailing newline, and two processes appending
        // to the same session file can interleave between them, gluing
        // two records onto one line. A single O_APPEND write keeps each
        // record atomic on its own line.
        let mut buf = serde_json::to_string(line)?;
        buf.push('\n');
        file.write_all(buf.as_bytes())?;
        // Tiered durability: turn boundaries and branch navigation are the
        // milestones a resume depends on, so `ExchangeEnd` and `Leaf` fsync
        // — and an fsync flushes every earlier line of the turn with them.
        // Mid-turn appends (messages, repls, `ExchangeStart`) only reach
        // the page cache: they survive a process crash, and an OS/power
        // crash truncating them is the designed interrupted-exchange
        // signature, not corruption.
        if matches!(line, Line::ExchangeEnd(_) | Line::Leaf(_)) {
            file.sync_data()?;
        }
        Ok(match line {
            Line::Message(entry) => entry.id.clone(),
            Line::Session(header) => header.id.clone(),
            Line::Repl(entry) => entry.id.clone(),
            Line::Compaction(entry) => entry.id.clone(),
            Line::Leaf(record) => record.id.clone().unwrap_or_default(),
            Line::ExchangeStart(start) => start.id.clone(),
            Line::ExchangeEnd(end) => end.id.clone(),
        })
    }

    /// Rewrite the log without records that no longer affect a load:
    /// superseded `Leaf` lines and closed exchange-marker pairs. Every
    /// message and repl line is live (they form the branch tree) and is
    /// preserved verbatim; an unacknowledged interrupted exchange keeps
    /// its open `ExchangeStart` so the crash signature survives. Writes
    /// to a temp file and renames into place, so a crash mid-compact
    /// leaves the original untouched. Only runs when it would reclaim at
    /// least `COMPACT_MIN_SAVED_LINES`; returns whether it ran.
    ///
    /// Call when opening a session for use (resume), not while listing:
    /// listing must never rewrite files behind the user's back.
    pub fn compact(&mut self) -> Result<bool> {
        const COMPACT_MIN_SAVED_LINES: usize = 64;
        if !self.persisted {
            return Ok(false);
        }
        let bytes = std::fs::read(&self.path)?;
        let current_lines = bytes.iter().filter(|&&b| b == b'\n').count();
        // A Leaf record is only needed when the effective leaf differs
        // from the implicit one (the last message line).
        let implicit_leaf = self.entries.last().map(|e| e.id.clone());
        let needs_leaf = !self.entries.is_empty() && self.leaf != implicit_leaf;
        let live_lines = 1 // header
            + self.entries.len()
            + self.repls.len()
            + self.compactions.len()
            + usize::from(needs_leaf)
            + usize::from(self.interrupted_exchange.is_some())
            + usize::from(self.open_exchange.is_some());
        if current_lines.saturating_sub(live_lines) < COMPACT_MIN_SAVED_LINES {
            return Ok(false);
        }

        let dir = self.path.parent().context("session path has a dir")?;
        let tmp_path = dir.join(format!("{}.jsonl.compact", self.header.id));
        {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&tmp_path)?;
            writeln!(
                file,
                "{}",
                serde_json::to_string(&Line::Session(self.header.clone()))?
            )?;
            for entry in &self.entries {
                writeln!(
                    file,
                    "{}",
                    serde_json::to_string(&Line::Message(entry.clone()))?
                )?;
            }
            for repl in &self.repls {
                writeln!(
                    file,
                    "{}",
                    serde_json::to_string(&Line::Repl(repl.clone()))?
                )?;
            }
            for compaction in &self.compactions {
                writeln!(
                    file,
                    "{}",
                    serde_json::to_string(&Line::Compaction(compaction.clone()))?
                )?;
            }
            // Chronological precedence on reload: the interrupted marker
            // first, then the currently open one (last start wins).
            if let Some(id) = &self.interrupted_exchange {
                writeln!(
                    file,
                    "{}",
                    serde_json::to_string(&Line::ExchangeStart(ExchangeStart { id: id.clone() }))?
                )?;
            }
            if let Some(id) = &self.open_exchange {
                writeln!(
                    file,
                    "{}",
                    serde_json::to_string(&Line::ExchangeStart(ExchangeStart { id: id.clone() }))?
                )?;
            }
            if needs_leaf {
                writeln!(
                    file,
                    "{}",
                    serde_json::to_string(&Line::Leaf(LeafRecord {
                        id: self.leaf.clone()
                    }))?
                )?;
            }
            file.sync_data()?;
        }
        // The compacted file is built from this process's in-memory
        // snapshot; if another process appended to the log since the size
        // check above, renaming over it would silently drop those records.
        // Abort instead — compaction is an optimization, losing data isn't.
        if std::fs::metadata(&self.path)?.len() != bytes.len() as u64 {
            let _ = std::fs::remove_file(&tmp_path);
            return Ok(false);
        }
        std::fs::rename(&tmp_path, &self.path)?;
        if let Ok(dir_handle) = std::fs::File::open(dir) {
            let _ = dir_handle.sync_data();
        }
        Ok(true)
    }

    /// New session containing the lineage up to (but not including)
    /// `entry_id`, pi-style fork: the excluded entry's text goes back into
    /// the editor.
    pub fn fork(&self, entry_id: &str) -> Result<SessionStore> {
        let entry = self
            .entry(entry_id)
            .with_context(|| format!("unknown session entry: {entry_id}"))?;
        let kept: Vec<super::types::Entry> = match &entry.parent_id {
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
                version: SESSION_VERSION,
                cwd: self.header.cwd.clone(),
                parent_session: Some(self.path.display().to_string()),
            },
            entries: Vec::new(),
            repls: Vec::new(),
            compactions: Vec::new(),
            leaf: kept.last().map(|e| e.id.clone()),
            persisted: false,
            open_exchange: None,
            interrupted_exchange: None,
            entry_children: HashMap::new(),
            repl_children: HashMap::new(),
        };
        for entry in kept {
            if let Some(pid) = &entry.parent_id {
                store
                    .entry_children
                    .entry(pid.clone())
                    .or_default()
                    .push(entry.id.clone());
            }
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
}
