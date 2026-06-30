//! Session discovery, listing, and age formatting.

use std::path::Path;

use super::store::sessions_dir;
use super::types::{Role, SessionMeta, SessionStore};

impl SessionStore {
    /// Sessions in `cwd`'s session dir, newest creation first.
    ///
    /// Sorted by session ID (a UUID v7 whose first 48 bits encode the
    /// creation timestamp), NOT by file modification time. This keeps the
    /// sidebar order stable across turns: every `SessionStore::append`
    /// writes to the JSONL file and trips the filesystem watcher, which
    /// calls `refresh_project` → `list`. If we sorted by `modified`, the
    /// session being written would jump to the top on every single
    /// turn/message. Sorting by creation order means sessions only move
    /// up when a new session is created (user sends a first message or an
    /// answer is submitted in a new session).
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
                    id: store.id().to_owned(),
                    message_count: store.entries.len(),
                    first_message,
                })
            })
            .collect();
        // UUID v7 is time-ordered: lexicographic descending sort puts
        // newest-created sessions first, without reshuffling on writes.
        sessions.sort_by(|a, b| b.id.cmp(&a.id));
        sessions
    }
}

/// Coarse relative age for the /resume list ("now", "5m", "3h", "2d").
pub fn age(modified: std::time::SystemTime) -> String {
    let secs = modified.elapsed().map(|d| d.as_secs()).unwrap_or(0);
    match secs {
        0..60 => "now".to_owned(),
        60..3600 => format!("{}m", secs / 60),
        3600..86_400 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}
