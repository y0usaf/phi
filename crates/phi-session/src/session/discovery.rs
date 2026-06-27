//! Session discovery, listing, and age formatting.

use std::path::Path;

use super::store::sessions_dir;
use super::types::{Role, SessionMeta, SessionStore};

impl SessionStore {
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
pub fn age(modified: std::time::SystemTime) -> String {
    let secs = modified.elapsed().map(|d| d.as_secs()).unwrap_or(0);
    match secs {
        0..60 => "now".to_owned(),
        60..3600 => format!("{}m", secs / 60),
        3600..86_400 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}
