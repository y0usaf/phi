//! Persisted mux workspace restore state (port of pi-harness's debounced
//! saver): open projects and the last selection, at
//! `{cache dir}/mux-state.json`. Saves are debounced 250ms on a background
//! thread and written atomically (tmp + rename); quit flushes synchronously.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

const SAVE_DEBOUNCE: Duration = Duration::from_millis(250);

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PersistedState {
    /// Open project directories, sidebar order.
    pub projects: Vec<String>,
    pub selected_project: Option<String>,
    /// Selected session's JSONL path.
    pub selected_session: Option<String>,
}

impl PersistedState {
    pub fn load_default() -> Result<Self> {
        Self::load_from(&default_state_path()?)
    }

    pub fn enqueue_save(&self) {
        if let Ok(path) = default_state_path() {
            let _ = save_queue().send(SaveCommand::Save {
                path,
                state: self.clone(),
            });
        }
    }

    /// Block until any pending debounced save has hit disk.
    pub fn flush() {
        flush_save_queue();
    }

    /// Dedup projects (keeping order, absolutized) and drop a selection that
    /// no longer points at an open project.
    pub fn normalize(&mut self) {
        let mut seen = HashSet::new();
        self.projects = self
            .projects
            .iter()
            .map(|p| normalize_path(Path::new(p)))
            .filter(|p| seen.insert(p.clone()))
            .collect();
        if let Some(selected) = &self.selected_project {
            let normalized = normalize_path(Path::new(selected));
            self.selected_project = self
                .projects
                .contains(&normalized)
                .then_some(normalized.clone());
            if self.selected_project.is_none() {
                self.selected_session = None;
            }
        }
    }

    fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let content =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let mut state: Self = serde_json::from_str(&content)
            .with_context(|| format!("parsing {}", path.display()))?;
        state.normalize();
        Ok(state)
    }

    fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut copy = self.clone();
        copy.normalize();
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&copy)?)?;
        std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }
}

/// Absolute, dot-free form of a project path (no symlink resolution, so the
/// user's spelling survives).
pub fn normalize_path(path: &Path) -> String {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut clean = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                clean.pop();
            }
            other => clean.push(other),
        }
    }
    clean.to_string_lossy().into_owned()
}

pub fn default_state_path() -> Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "phi")
        .context("could not determine a home directory")?;
    Ok(dirs.cache_dir().join("mux-state.json"))
}

enum SaveCommand {
    Save {
        path: PathBuf,
        state: PersistedState,
    },
    Flush(Sender<()>),
}

static SAVE_TX: OnceLock<Sender<SaveCommand>> = OnceLock::new();

fn save_queue() -> &'static Sender<SaveCommand> {
    SAVE_TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel();
        let _ = std::thread::Builder::new()
            .name("phi-mux-state-save".into())
            .spawn(move || {
                let mut pending: Option<(PathBuf, PersistedState)> = None;
                let mut deadline: Option<Instant> = None;
                loop {
                    let command = match deadline {
                        Some(at) => {
                            match rx.recv_timeout(at.saturating_duration_since(Instant::now())) {
                                Ok(command) => Some(command),
                                Err(RecvTimeoutError::Timeout) => {
                                    if let Some((path, state)) = pending.take() {
                                        let _ = state.save_to(&path);
                                    }
                                    deadline = None;
                                    continue;
                                }
                                Err(RecvTimeoutError::Disconnected) => None,
                            }
                        }
                        None => rx.recv().ok(),
                    };
                    match command {
                        Some(SaveCommand::Save { path, state }) => {
                            pending = Some((path, state));
                            deadline = Some(Instant::now() + SAVE_DEBOUNCE);
                        }
                        Some(SaveCommand::Flush(done)) => {
                            if let Some((path, state)) = pending.take() {
                                let _ = state.save_to(&path);
                            }
                            deadline = None;
                            let _ = done.send(());
                        }
                        None => {
                            if let Some((path, state)) = pending.take() {
                                let _ = state.save_to(&path);
                            }
                            break;
                        }
                    }
                }
            });
        tx
    })
}

fn flush_save_queue() {
    let (tx, rx) = mpsc::channel();
    if save_queue().send(SaveCommand::Flush(tx)).is_ok() {
        let _ = rx.recv();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_load_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state.json");
        let state = PersistedState {
            projects: vec!["/tmp/a".into(), "/tmp/b".into()],
            selected_project: Some("/tmp/b".into()),
            selected_session: Some("/tmp/s.jsonl".into()),
        };
        state.save_to(&path).unwrap();
        assert_eq!(PersistedState::load_from(&path).unwrap(), state);
    }

    #[test]
    fn missing_file_loads_default() {
        let tmp = tempfile::tempdir().unwrap();
        let loaded = PersistedState::load_from(&tmp.path().join("nope.json")).unwrap();
        assert_eq!(loaded, PersistedState::default());
    }

    #[test]
    fn normalize_dedups_and_drops_dangling_selection() {
        let mut state = PersistedState {
            projects: vec!["/tmp/a".into(), "/tmp/x/../a".into(), "/tmp/b".into()],
            selected_project: Some("/tmp/missing".into()),
            selected_session: Some("/tmp/s.jsonl".into()),
        };
        state.normalize();
        assert_eq!(state.projects, vec!["/tmp/a", "/tmp/b"]);
        assert_eq!(state.selected_project, None);
        assert_eq!(state.selected_session, None);
    }

    #[test]
    fn normalize_keeps_session_when_project_is_valid() {
        let mut state = PersistedState {
            projects: vec!["/tmp/a".into()],
            selected_project: Some("/tmp/x/../a".into()),
            selected_session: Some("/tmp/s.jsonl".into()),
        };
        state.normalize();
        assert_eq!(state.selected_project, Some("/tmp/a".into()));
        assert_eq!(state.selected_session, Some("/tmp/s.jsonl".into()));
    }
}
