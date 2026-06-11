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
}

pub struct SessionStore {
    path: PathBuf,
    header: Header,
    entries: Vec<Entry>,
    /// Tip of the active branch; new entries attach here.
    leaf: Option<String>,
    /// Whether the file (header) has been written yet.
    persisted: bool,
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
    let dirs = directories::ProjectDirs::from("", "", "phi")
        .context("could not determine a home directory")?;
    let safe = format!(
        "--{}--",
        cwd.display()
            .to_string()
            .trim_start_matches(['/', '\\'])
            .replace(['/', '\\', ':'], "-")
    );
    Ok(dirs.data_dir().join("sessions").join(safe))
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
            leaf: None,
            persisted: false,
        })
    }

    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read session: {}", path.display()))?;
        let mut header = None;
        let mut entries = Vec::new();
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            // Skip malformed lines (pi-style) so one bad write doesn't lose
            // the whole session.
            match serde_json::from_str::<Line>(line) {
                Ok(Line::Session(h)) if header.is_none() => header = Some(h),
                Ok(Line::Message(entry)) => entries.push(entry),
                _ => {}
            }
        }
        let header = header
            .with_context(|| format!("not a phi session file (no header): {}", path.display()))?;
        let leaf = entries.last().map(|e| e.id.clone());
        Ok(Self {
            path: path.to_owned(),
            header,
            entries,
            leaf,
            persisted: true,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn id(&self) -> &str {
        &self.header.id
    }

    pub fn leaf(&self) -> Option<&str> {
        self.leaf.as_deref()
    }

    pub fn entry(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// Move the active branch tip (tree navigation). `None` rewinds to the
    /// session root.
    pub fn set_leaf(&mut self, id: Option<String>) {
        self.leaf = id;
    }

    /// Append a message on the active branch and persist it.
    pub fn append(&mut self, role: Role, text: &str) -> Result<String> {
        let entry = Entry {
            id: new_id(),
            parent_id: self.leaf.clone(),
            timestamp: now(),
            role,
            text: text.to_owned(),
        };
        self.leaf = Some(entry.id.clone());
        self.entries.push(entry.clone());
        self.write_line(&Line::Message(entry))
    }

    fn write_line(&mut self, line: &Line) -> Result<String> {
        if !self.persisted {
            std::fs::create_dir_all(self.path.parent().expect("session path has a dir"))?;
            std::fs::write(
                &self.path,
                format!(
                    "{}\n",
                    serde_json::to_string(&Line::Session(self.header.clone()))?
                ),
            )?;
            self.persisted = true;
        }
        let mut file = std::fs::OpenOptions::new().append(true).open(&self.path)?;
        writeln!(file, "{}", serde_json::to_string(line)?)?;
        Ok(match line {
            Line::Message(entry) => entry.id.clone(),
            Line::Session(header) => header.id.clone(),
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

    /// (prompt, response) pairs on the active branch, for rebuilding the RLM
    /// env history. Prompts without a recorded response (failed runs) are
    /// skipped, matching what the live env would have stored.
    pub fn branch_exchanges(&self) -> Vec<(String, String)> {
        let branch = self.branch();
        let mut pairs = Vec::new();
        let mut iter = branch.iter().peekable();
        while let Some(entry) = iter.next() {
            if entry.role != Role::User {
                continue;
            }
            if let Some(next) = iter.peek()
                && next.role == Role::Assistant
            {
                pairs.push((entry.text.clone(), next.text.clone()));
            }
        }
        pairs
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
        let mut store = SessionStore {
            path,
            header: Header {
                id,
                timestamp: now(),
                cwd: self.header.cwd.clone(),
                parent_session: Some(self.path.display().to_string()),
            },
            entries: Vec::new(),
            leaf: kept.last().map(|e| e.id.clone()),
            persisted: false,
        };
        for entry in kept {
            store.entries.push(entry.clone());
            store.write_line(&Line::Message(entry))?;
        }
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
        assert_eq!(
            loaded.branch_exchanges(),
            vec![
                ("first prompt".to_owned(), "first answer".to_owned()),
                ("second prompt".to_owned(), "second answer".to_owned()),
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

        assert_eq!(
            store.branch_exchanges(),
            vec![
                ("q1".to_owned(), "a1".to_owned()),
                ("q2-alt".to_owned(), "a2-alt".to_owned()),
            ]
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
        assert!(store.branch_exchanges().is_empty());
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
        assert_eq!(
            fork.branch_exchanges(),
            vec![("q1".to_owned(), "a1".to_owned())]
        );
        // Forking from the first message yields an empty session.
        let first = store.user_messages()[0].id.clone();
        let empty = store.fork(&first).unwrap();
        assert!(empty.branch().is_empty());
    }
}
