//! Filesystem watcher for session directories. Watches each active
//! project's session folder and notifies the main loop when files change,
//! so the sidebar stays canonical with the filesystem as source of truth.

use std::collections::HashMap;
use std::path::PathBuf;

use notify::{EventHandler, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use phi_session::session::sessions_dir;
use tokio::sync::mpsc;

/// Watches session directories for each active project. On filesystem
/// events (create/modify/remove of `.jsonl` files), sends the project index
/// through an unbounded channel so the main loop can debounce and refresh.
pub struct SessionWatcher {
    /// Maps project index to its watcher + the directory it watches.
    watchers: HashMap<usize, (RecommendedWatcher, PathBuf)>,
    /// Channel for notifying the main loop of changes.
    tx: mpsc::UnboundedSender<usize>,
}

/// Event payload sent to the main loop: which project index had a change.
pub type WatchEvent = usize;

impl SessionWatcher {
    /// Create a new watcher set. The returned receiver delivers project
    /// indices whose session directory changed.
    pub fn new() -> (Self, mpsc::UnboundedReceiver<WatchEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let watcher = Self {
            watchers: HashMap::new(),
            tx,
        };
        (watcher, rx)
    }

    /// Start watching a project's session directory. Idempotent: if a
    /// watcher for this index already exists, it is replaced.
    pub fn add_watch(&mut self, index: usize, cwd: &PathBuf) {
        let dir = match sessions_dir(cwd) {
            Ok(dir) => dir,
            Err(_) => return,
        };

        // Ensure the directory exists so the watcher can observe it.
        let _ = std::fs::create_dir_all(&dir);

        let tx = self.tx.clone();
        let index = index;
        let handler = SessionEventHandler { tx, index };

        match notify::recommended_watcher(handler) {
            Ok(mut watcher) => {
                if let Err(e) = watcher.watch(&dir, RecursiveMode::NonRecursive) {
                    eprintln!("watcher: failed to watch {}: {e}", dir.display());
                    return;
                }
                self.watchers.insert(index, (watcher, dir));
            }
            Err(e) => {
                eprintln!("watcher: failed to create watcher for {}: {e}", dir.display());
            }
        }
    }

    /// Stop watching a project's session directory.
    pub fn remove_watch(&mut self, index: usize) {
        self.watchers.remove(&index);
    }

    /// Rebuild watchers to match a new set of (index, cwd) pairs. Drops
    /// watchers for indices no longer present, adds watchers for new ones.
    pub fn sync(&mut self, projects: &[(usize, &PathBuf)]) {
        let new_indices: std::collections::HashSet<usize> =
            projects.iter().map(|(i, _)| *i).collect();
        self.watchers.retain(|index, _| new_indices.contains(index));
        for (index, cwd) in projects {
            let needs_watch = match self.watchers.get(index) {
                None => true,
                Some((_, existing_dir)) => match (sessions_dir(cwd), sessions_dir(existing_dir)) {
                    (Ok(new_dir), Ok(old_dir)) => new_dir != old_dir,
                    _ => true,
                },
            };
            if needs_watch {
                self.add_watch(*index, *cwd);
            }
        }
    }

    /// Remove all watchers.
    pub fn clear(&mut self) {
        self.watchers.clear();
    }
}

/// Event handler that forwards relevant filesystem events to the main loop.
struct SessionEventHandler {
    tx: mpsc::UnboundedSender<usize>,
    index: usize,
}

impl EventHandler for SessionEventHandler {
    fn handle_event(&mut self, event: Result<notify::Event, notify::Error>) {
        if let Ok(event) = event {
            let relevant = matches!(
                event.kind,
                EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
            );
            if !relevant {
                return;
            }
            let jsonl_related = event
                .paths
                .iter()
                .any(|p| p.extension().is_some_and(|ext| ext == "jsonl"));
            if jsonl_related {
                let _ = self.tx.send(self.index);
            }
        }
    }
}
