//! Session persistence: file I/O, append-only writes, and path helpers.
//!
//! Files live under `{root}/{encoded-cwd}/{timestamp}_{id}.jsonl`, mirroring
//! pi's per-cwd layout (the cwd is flattened into a safe directory name so
//! sessions for different projects don't collide). The tolerant reader
//! skips malformed lines so a torn final write from a crash doesn't lose
//! the session.
//!
//! The sessions root is an explicit argument to the store, list, and
//! most-recent functions so tests can point it at a temp dir and the app
//! can compute it once (the phi data dir) and thread it. [`sessions_root`]
//! is the app's default root.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};

use crate::types::{
    Entry, Header, Line, SESSION_VERSION, SessionMeta, SessionStore, new_id, now_timestamp,
};

/// Default root of all per-cwd session directories: `{data dir}/sessions`.
pub fn sessions_root() -> Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "phi").context("no home directory")?;
    Ok(dirs.data_dir().join("sessions"))
}

/// `--{cwd with separators flattened}--`, the per-cwd directory name (pi's
/// encoding, so a future shared session picker could browse the same tree).
pub fn flatten_cwd(cwd: &Path) -> String {
    let raw = cwd.display().to_string();
    let stripped = raw.trim_start_matches(['/', '\\']);
    format!("--{}--", stripped.replace(['/', '\\', ':'], "-"))
}

/// Per-cwd session directory under `root`: `{root}/{flatten_cwd(cwd)}`.
pub fn sessions_dir_in(root: &Path, cwd: &Path) -> PathBuf {
    root.join(flatten_cwd(cwd))
}

/// Per-cwd session directory under the default root. Convenience wrapper
/// around [`sessions_dir_in`] for the app's common case.
pub fn sessions_dir(cwd: &Path) -> Result<PathBuf> {
    Ok(sessions_dir_in(&sessions_root()?, cwd))
}

impl SessionStore {
    /// Create a new session file under `root` and write its header. The
    /// file is named `{timestamp}_{id}.jsonl` (colons/dots in the timestamp
    /// flattened to dashes, matching pi) so a plain `ls` is chronologically
    /// ordered.
    pub fn create_in(root: &Path, cwd: &Path, model: &str) -> Result<Self> {
        let id = new_id();
        let timestamp = now_timestamp();
        let dir = sessions_dir_in(root, cwd);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("creating session dir {}", dir.display()))?;
        let file_ts = timestamp.replace([':', '.'], "-");
        let path = dir.join(format!("{file_ts}_{id}.jsonl"));
        let header = Header {
            version: SESSION_VERSION,
            id,
            timestamp,
            cwd: cwd.display().to_string(),
            model: model.to_owned(),
            parent_session: None,
        };
        let line = serde_json::to_string(&Line::Session(header.clone()))
            .context("encoding session header")?;
        std::fs::write(&path, format!("{line}\n"))
            .with_context(|| format!("writing session header {}", path.display()))?;
        Ok(Self {
            path,
            header,
            count: 0,
            first_message: None,
        })
    }

    /// Create under the default root ([`sessions_root`]).
    pub fn create(cwd: &Path, model: &str) -> Result<Self> {
        Self::create_in(&sessions_root()?, cwd, model)
    }

    /// Open an existing session file and read its header + message count
    /// (the transcript itself is returned by [`load_messages`]; this is the
    /// metadata path used by `--continue` and the `/resume` picker).
    pub fn open(path: &Path) -> Result<Self> {
        let (header, count, first_message) = read_header_and_stats(path)?;
        Ok(Self {
            path: path.to_owned(),
            header,
            count,
            first_message,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn id(&self) -> &str {
        &self.header.id
    }

    pub fn header(&self) -> &Header {
        &self.header
    }

    pub fn message_count(&self) -> usize {
        self.count
    }

    /// Append one transcript message as its own JSONL line. Cheap and
    /// crash-safe: a single `write` per message, so a torn final line is
    /// the worst a crash can do (the reader skips it).
    pub fn append(&mut self, message: &phi_ai_types::Message) -> Result<()> {
        let entry = Entry {
            message: message.clone(),
        };
        let line = serde_json::to_string(&Line::Message(entry)).context("encoding message")?;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&self.path)
            .with_context(|| format!("opening session {}", self.path.display()))?;
        writeln!(file, "{line}")
            .with_context(|| format!("writing session {}", self.path.display()))?;
        self.count += 1;
        if self.first_message.is_none()
            && let phi_ai_types::Message::User(u) = message
        {
            self.first_message = Some(u.content.clone());
        }
        Ok(())
    }

    /// Append every message in a transcript slice (turns flush their final
    /// messages through this in one call).
    pub fn append_all(&mut self, messages: &[phi_ai_types::Message]) -> Result<()> {
        for message in messages {
            self.append(message)?;
        }
        Ok(())
    }
}

/// Read the whole session file: header + every loadable message entry, in
/// file order. Malformed lines (including a torn final write) are skipped,
/// so the transcript is whatever survived.
pub fn load_messages(path: &Path) -> Result<(Header, Vec<phi_ai_types::Message>)> {
    let bytes =
        std::fs::read(path).with_context(|| format!("reading session {}", path.display()))?;
    let content = String::from_utf8_lossy(&bytes);
    let mut header: Option<Header> = None;
    let mut messages = Vec::new();
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        // Stream-deserialize so a historical multi-record glue on one line
        // (pi hit this once) still loads each record it can.
        let mut records = serde_json::Deserializer::from_str(line).into_iter::<Line>();
        while let Some(Ok(record)) = records.next() {
            match record {
                Line::Session(h) if header.is_none() => {
                    if h.version > SESSION_VERSION {
                        bail!(
                            "session file version {} is newer than this phi understands (max {}): {}",
                            h.version,
                            SESSION_VERSION,
                            path.display()
                        );
                    }
                    header = Some(h);
                }
                Line::Session(_) => {} // a second header is ignored
                Line::Message(entry) => messages.push(entry.message),
            }
        }
    }
    let header = header
        .with_context(|| format!("not a phi session file (no header): {}", path.display()))?;
    Ok((header, messages))
}

/// Header + message count + first user prompt — the metadata path used by
/// `--continue` and the `/resume` picker, without holding the transcript.
fn read_header_and_stats(path: &Path) -> Result<(Header, usize, Option<String>)> {
    let (header, messages) = load_messages(path)?;
    let first = messages.iter().find_map(|m| match m {
        phi_ai_types::Message::User(u) => Some(u.content.clone()),
        _ => None,
    });
    Ok((header, messages.len(), first))
}

/// All sessions for `cwd`'s directory under `root`, newest creation first
/// (UUID v7 is time-ordered, so lexicographic descending sort is creation
/// order — stable across writes, unlike mtime).
pub fn list_in(root: &Path, cwd: &Path) -> Vec<SessionMeta> {
    let dir = sessions_dir_in(root, cwd);
    let Ok(read) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut sessions: Vec<SessionMeta> = read
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|e| {
            let path = e.path();
            let store = SessionStore::open(&path).ok()?;
            if store.count == 0 {
                return None;
            }
            Some(SessionMeta {
                path,
                id: store.header.id.clone(),
                model: store.header.model.clone(),
                cwd: store.header.cwd.clone(),
                first_message: store.first_message.unwrap_or_default(),
                message_count: store.count,
                created_at: store.header.timestamp.clone(),
            })
        })
        .collect();
    sessions.sort_by(|a, b| b.id.cmp(&a.id));
    sessions
}

/// All sessions for `cwd` under the default root.
pub fn list(cwd: &Path) -> Vec<SessionMeta> {
    match sessions_root() {
        Ok(root) => list_in(&root, cwd),
        Err(_) => Vec::new(),
    }
}

/// The most recent session for `cwd` under `root`, or `None` when there is
/// none (or the directory is unreadable). Used by `--continue`.
pub fn most_recent_in(root: &Path, cwd: &Path) -> Option<PathBuf> {
    list_in(root, cwd).into_iter().next().map(|m| m.path)
}

/// The most recent session for `cwd` under the default root.
pub fn most_recent(cwd: &Path) -> Option<PathBuf> {
    match sessions_root() {
        Ok(root) => most_recent_in(&root, cwd),
        Err(_) => None,
    }
}
