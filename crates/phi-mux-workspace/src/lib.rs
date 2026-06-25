//! Mux workspace: the projects and sessions shown in the sidebar, plus
//! archive/restore of session files. Sessions are discovered from the
//! on-disk session store; embedded handles are attached lazily when a
//! session is first focused.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context as _, Result};

use phi_mux_driver::{SessionHandle, SessionKey};
use phi_mux_status::SessionStatus;
use phi_session::session::{self, SessionStore};

pub struct Workspace {
    pub projects: Vec<MuxProject>,
    pub selected_project: usize,
    next_key: SessionKey,
}

pub struct MuxProject {
    pub cwd: PathBuf,
    pub sessions: Vec<MuxSession>,
    pub selected: usize,
}

pub struct MuxSession {
    pub key: SessionKey,
    /// JSONL file backing the session; `None` for a fresh draft (the store
    /// is created lazily on the first message).
    pub store_path: Option<PathBuf>,
    pub first_message: String,
    pub modified: Option<SystemTime>,
    pub status: SessionStatus,
    pub handle: Option<SessionHandle>,
}

impl MuxSession {
    pub fn is_draft(&self) -> bool {
        self.store_path.is_none()
    }

    pub fn is_sidebar_visible(&self) -> bool {
        !self.is_draft() || !self.first_message.trim().is_empty()
    }

    pub fn is_unmaterialized_draft(&self) -> bool {
        self.is_draft() && !self.is_sidebar_visible()
    }
}

impl Workspace {
    fn empty() -> Self {
        Self {
            projects: Vec::new(),
            selected_project: 0,
            next_key: 0,
        }
    }

    /// Discover sessions for each project directory (newest first).
    pub fn discover(project_dirs: Vec<PathBuf>) -> Self {
        let mut ws = Self::empty();
        for cwd in project_dirs {
            ws.add_project(cwd);
        }
        ws
    }

    fn next_key(&mut self) -> SessionKey {
        self.next_key += 1;
        self.next_key
    }

    /// Add a project (idempotent); returns its index.
    pub fn add_project(&mut self, cwd: PathBuf) -> usize {
        if let Some(i) = self.projects.iter().position(|p| p.cwd == cwd) {
            return i;
        }
        let sessions = SessionStore::list(&cwd)
            .into_iter()
            .map(|meta| MuxSession {
                key: 0,
                store_path: Some(meta.path),
                first_message: meta.first_message,
                modified: Some(meta.modified),
                status: SessionStatus::Idle,
                handle: None,
            })
            .collect::<Vec<_>>();
        let mut project = MuxProject {
            cwd,
            sessions,
            selected: 0,
        };
        for s in &mut project.sessions {
            s.key = self.next_key;
            self.next_key += 1;
        }
        self.projects.push(project);
        self.projects.len() - 1
    }

    /// Drop a project from the workspace (files untouched); running session
    /// tasks abort when their handles drop.
    pub fn remove_project(&mut self, index: usize) {
        if index < self.projects.len() {
            self.projects.remove(index);
            if self.selected_project >= self.projects.len() {
                self.selected_project = self.projects.len().saturating_sub(1);
            }
        }
    }

    /// Rescan a project's session directory, keeping live handles, drafts,
    /// and statuses for sessions that still exist.
    pub fn refresh_project(&mut self, index: usize) {
        let Some(project) = self.projects.get_mut(index) else {
            return;
        };
        let selected_key = project.sessions.get(project.selected).map(|s| s.key);
        let mut old = std::mem::take(&mut project.sessions);
        let mut fresh: Vec<MuxSession> = Vec::new();
        // Drafts (no file yet) stay; a draft that materialized since the
        // last scan is matched below by its running handle's path.
        for meta in SessionStore::list(&project.cwd) {
            let existing = old.iter().position(|s| {
                s.store_path.as_deref() == Some(meta.path.as_path())
                    || (s.is_draft()
                        && s.handle.as_ref().is_some_and(|h| h.store_path == meta.path))
            });
            match existing {
                Some(i) => {
                    let mut s = old.remove(i);
                    s.store_path = Some(meta.path);
                    s.first_message = meta.first_message;
                    s.modified = Some(meta.modified);
                    fresh.push(s);
                }
                None => fresh.push(MuxSession {
                    key: 0,
                    store_path: Some(meta.path),
                    first_message: meta.first_message,
                    modified: Some(meta.modified),
                    status: SessionStatus::Idle,
                    handle: None,
                }),
            }
        }
        // Keep drafts and anything with a live handle (e.g. a session whose
        // file was archived while running). A stale draft — no file, no live
        // handle — means the session was archived or its file was removed
        // externally, so drop it on refresh. The key-0 unmaterialized draft
        // is retained so `new_draft` can reuse it.
        fresh.extend(
            old.into_iter()
                .filter(|s| s.handle.is_some() || (s.is_draft() && s.key == 0)),
        );
        for s in &mut fresh {
            if s.key == 0 {
                s.key = self.next_key;
                self.next_key += 1;
            }
        }
        project.sessions = fresh;
        project.selected = selected_key
            .and_then(|key| project.sessions.iter().position(|s| s.key == key))
            .unwrap_or(0);
    }

    /// New draft session at the top of the project; reuses an existing
    /// untouched draft instead of stacking empties.
    pub fn new_draft(&mut self, project_index: usize) -> Option<&mut MuxSession> {
        let key = self.next_key();
        let project = self.projects.get_mut(project_index)?;
        if let Some(i) = project
            .sessions
            .iter()
            .position(MuxSession::is_unmaterialized_draft)
        {
            project.selected = i;
            return project.sessions.get_mut(i);
        }
        project.sessions.insert(
            0,
            MuxSession {
                key,
                store_path: None,
                first_message: String::new(),
                modified: None,
                status: SessionStatus::Idle,
                handle: None,
            },
        );
        project.selected = 0;
        project.sessions.first_mut()
    }

    pub fn ensure_project_session(&mut self, project_index: usize) -> Option<&mut MuxSession> {
        if self.projects.get(project_index)?.sessions.is_empty() {
            return self.new_draft(project_index);
        }
        let project = self.projects.get_mut(project_index)?;
        if project.selected >= project.sessions.len() {
            project.selected = project.sessions.len().saturating_sub(1);
        }
        project.sessions.get_mut(project.selected)
    }

    pub fn visible_session_indices(&self, project_index: usize) -> Vec<usize> {
        self.projects
            .get(project_index)
            .map(|project| {
                project
                    .sessions
                    .iter()
                    .enumerate()
                    .filter_map(|(i, session)| session.is_sidebar_visible().then_some(i))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn normalize_visible_selection(&mut self, project_index: usize) -> bool {
        let Some(project) = self.projects.get_mut(project_index) else {
            return false;
        };
        if project.sessions.is_empty() {
            project.selected = 0;
            return false;
        }
        if project.selected >= project.sessions.len() {
            project.selected = project.sessions.len().saturating_sub(1);
        }
        if project
            .sessions
            .get(project.selected)
            .is_some_and(MuxSession::is_sidebar_visible)
        {
            return false;
        }

        let current = project.selected;
        let replacement = project
            .sessions
            .iter()
            .enumerate()
            .skip(current)
            .find_map(|(i, session)| session.is_sidebar_visible().then_some(i))
            .or_else(|| {
                project
                    .sessions
                    .iter()
                    .enumerate()
                    .take(current)
                    .rev()
                    .find_map(|(i, session)| session.is_sidebar_visible().then_some(i))
            });

        match replacement {
            Some(index) => {
                let changed = index != project.selected;
                project.selected = index;
                changed
            }
            None => false,
        }
    }

    pub fn move_selected_visible_session(&mut self, project_index: usize, delta: i32) -> bool {
        if delta == 0 {
            return false;
        }
        let Some(project) = self.projects.get_mut(project_index) else {
            return false;
        };
        let visible = project
            .sessions
            .iter()
            .enumerate()
            .filter_map(|(i, session)| session.is_sidebar_visible().then_some(i))
            .collect::<Vec<_>>();
        if visible.is_empty() {
            return false;
        }

        let current = project
            .selected
            .min(project.sessions.len().saturating_sub(1));
        let steps = delta.unsigned_abs() as usize;
        let target_visible = match visible.iter().position(|&i| i == current) {
            Some(current_visible) if delta > 0 => (current_visible + steps) % visible.len(),
            Some(current_visible) => {
                (current_visible + visible.len() - (steps % visible.len())) % visible.len()
            }
            None if delta > 0 => {
                let base = visible.iter().position(|&i| i > current).unwrap_or(0);
                (base + steps - 1) % visible.len()
            }
            None => {
                let base = visible
                    .iter()
                    .rposition(|&i| i < current)
                    .unwrap_or(visible.len().saturating_sub(1));
                (base + visible.len() - ((steps - 1) % visible.len())) % visible.len()
            }
        };

        let next = visible[target_visible];
        let changed = next != project.selected;
        project.selected = next;
        changed
    }

    pub fn discard_unmaterialized_draft(&mut self, project_index: usize) -> bool {
        let Some(project) = self.projects.get_mut(project_index) else {
            return false;
        };
        let Some(index) = project.sessions.get(project.selected).and_then(|session| {
            session
                .is_unmaterialized_draft()
                .then_some(project.selected)
        }) else {
            return false;
        };
        project.sessions.remove(index);
        if !project.sessions.is_empty() {
            project.selected = project
                .selected
                .min(project.sessions.len().saturating_sub(1));
        }
        true
    }
}

/// An archived session file, for the archive overlay.
#[derive(Clone, Debug)]
pub struct ArchivedSession {
    pub path: PathBuf,
    pub cwd: PathBuf,
    pub first_message: String,
    pub modified: Option<SystemTime>,
}

pub fn archive_dir() -> Result<PathBuf> {
    Ok(session::sessions_root()?.join("ARCHIVE"))
}

/// Move a session JSONL into the archive as
/// `{original flattened dir}{uuid}.jsonl` (restore reads the header's cwd).
pub fn archive_session(path: &Path) -> Result<PathBuf> {
    archive_into(&archive_dir()?, path)
}

fn archive_into(archive: &Path, path: &Path) -> Result<PathBuf> {
    let dir_name = path
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let file_name = path
        .file_name()
        .context("session path has no file name")?
        .to_string_lossy();
    std::fs::create_dir_all(archive)?;
    let target = archive.join(format!("{dir_name}{file_name}"));
    rename_or_copy_delete(path, &target)
        .with_context(|| format!("archiving {} -> {}", path.display(), target.display()))?;
    Ok(target)
}

/// Move an archived session back to its project's session directory, using
/// the cwd recorded in the header.
pub fn restore_session(archived: &Path) -> Result<PathBuf> {
    restore_into(&session::sessions_root()?, archived)
}

fn restore_into(sessions_root: &Path, archived: &Path) -> Result<PathBuf> {
    let store = SessionStore::load(archived)?;
    let cwd = PathBuf::from(store.cwd());
    let dir = sessions_root.join(session::flatten_cwd(&cwd));
    std::fs::create_dir_all(&dir)?;
    let target = dir.join(format!("{}.jsonl", store.id()));
    rename_or_copy_delete(archived, &target)
        .with_context(|| format!("restoring {} -> {}", archived.display(), target.display()))?;
    Ok(target)
}

/// Move `src` to `target`; fall back to copy+delete when rename fails
/// (e.g. cross-device EXDEV), so the source survives only after the
/// destination is fully written.
fn rename_or_copy_delete(src: &Path, target: &Path) -> Result<()> {
    match std::fs::rename(src, target) {
        Ok(()) => Ok(()),
        Err(_) => {
            std::fs::copy(src, target)
                .with_context(|| format!("copying {} -> {}", src.display(), target.display()))?;
            std::fs::remove_file(src)
                .with_context(|| format!("removing source {}", src.display()))?;
            Ok(())
        }
    }
}

/// Archived sessions, newest first.
pub fn list_archived() -> Vec<ArchivedSession> {
    list_archived_in(&match archive_dir() {
        Ok(dir) => dir,
        Err(_) => return Vec::new(),
    })
}

fn list_archived_in(archive: &Path) -> Vec<ArchivedSession> {
    let Ok(read) = std::fs::read_dir(archive) else {
        return Vec::new();
    };
    let mut sessions: Vec<ArchivedSession> = read
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|e| {
            let path = e.path();
            let store = SessionStore::load(&path).ok()?;
            let first_message = store
                .user_messages()
                .first()
                .map(|m| m.text.clone())
                .unwrap_or_default();
            Some(ArchivedSession {
                cwd: PathBuf::from(store.cwd()),
                first_message,
                modified: e.metadata().ok().and_then(|m| m.modified().ok()),
                path,
            })
        })
        .collect();
    sessions.sort_by_key(|s| std::cmp::Reverse(s.modified));
    sessions
}

/// Delete archived sessions older than `max_age`; returns how many were
/// removed.
pub fn cleanup_archive(max_age: Duration) -> Result<usize> {
    cleanup_archive_in(&archive_dir()?, max_age)
}

fn cleanup_archive_in(archive: &Path, max_age: Duration) -> Result<usize> {
    let mut removed = 0;
    let Ok(read) = std::fs::read_dir(archive) else {
        return Ok(0);
    };
    for entry in read.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|x| x != "jsonl") {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age > max_age);
        if old && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use phi_session::session::Role;

    /// A real store written under `root/sessions/{flattened}/` to mimic the
    /// production layout.
    fn write_session(root: &Path, cwd: &Path, messages: &[&str]) -> PathBuf {
        let mut store = SessionStore::new(cwd).unwrap();
        let dir = root.join(session::flatten_cwd(cwd));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{}.jsonl", store.id()));
        store.set_test_path(path.clone());
        for (i, text) in messages.iter().enumerate() {
            let role = if i % 2 == 0 {
                Role::User
            } else {
                Role::Assistant
            };
            store.append(role, text).unwrap();
        }
        path
    }

    #[test]
    fn archive_and_restore_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let sessions_root = tmp.path().join("sessions");
        let cwd = tmp.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();
        let path = write_session(&sessions_root, &cwd, &["hello", "world"]);

        let archive = sessions_root.join("ARCHIVE");
        let archived = archive_into(&archive, &path).unwrap();
        assert!(!path.exists());
        assert!(archived.exists());
        let name = archived.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with("--"),
            "archived name keeps the dir: {name}"
        );

        let listed = list_archived_in(&archive);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].first_message, "hello");
        assert_eq!(listed[0].cwd, cwd);

        let restored = restore_into(&sessions_root, &archived).unwrap();
        assert_eq!(restored, path);
        assert!(restored.exists());
        assert!(!archived.exists());
    }

    #[test]
    fn cleanup_removes_only_old_archives() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("ARCHIVE");
        std::fs::create_dir_all(&archive).unwrap();
        std::fs::write(archive.join("fresh.jsonl"), "{}").unwrap();
        let removed = cleanup_archive_in(&archive, Duration::from_secs(3600)).unwrap();
        assert_eq!(removed, 0);
        let removed = cleanup_archive_in(&archive, Duration::ZERO).unwrap();
        assert_eq!(removed, 1);
        assert!(!archive.join("fresh.jsonl").exists());
    }

    #[test]
    fn draft_lifecycle_reuses_untouched_draft() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ws = Workspace::discover(vec![tmp.path().to_path_buf()]);
        assert_eq!(ws.projects.len(), 1);
        assert!(ws.projects[0].sessions.is_empty());

        ws.new_draft(0).unwrap();
        assert_eq!(ws.projects[0].sessions.len(), 1);
        assert!(ws.projects[0].sessions[0].is_unmaterialized_draft());
        assert!(ws.visible_session_indices(0).is_empty());
        // A second ctrl+n reuses the idle draft instead of stacking.
        ws.new_draft(0).unwrap();
        assert_eq!(ws.projects[0].sessions.len(), 1);
        assert!(ws.projects[0].sessions[0].is_draft());
    }

    #[test]
    fn refresh_project_drops_stale_draft_after_archive() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ws = Workspace::discover(vec![tmp.path().to_path_buf()]);
        // Simulate a session whose file was archived: no store_path, no handle.
        ws.projects[0].sessions = vec![test_session(1, None, "archived")];
        assert!(ws.projects[0].sessions[0].is_draft());

        // Ctrl+R / refresh should drop the stale draft — its file is gone.
        ws.refresh_project(0);
        assert!(
            ws.projects[0].sessions.is_empty(),
            "stale draft should be dropped on refresh"
        );
    }

    #[test]
    fn discard_unmaterialized_draft_removes_selected_draft() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ws = Workspace::discover(vec![tmp.path().to_path_buf()]);
        ws.new_draft(0).unwrap();
        assert!(ws.discard_unmaterialized_draft(0));
        assert!(ws.projects[0].sessions.is_empty());
    }

    fn test_session(key: SessionKey, path: Option<PathBuf>, first_message: &str) -> MuxSession {
        MuxSession {
            key,
            store_path: path,
            first_message: first_message.to_owned(),
            modified: None,
            status: SessionStatus::Idle,
            handle: None,
        }
    }

    #[test]
    fn normalize_visible_selection_prefers_next_visible_then_previous() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ws = Workspace::discover(vec![tmp.path().to_path_buf()]);
        ws.projects[0].sessions = vec![
            test_session(1, Some(tmp.path().join("a.jsonl")), "a"),
            test_session(2, None, ""),
            test_session(3, Some(tmp.path().join("c.jsonl")), "c"),
        ];

        ws.projects[0].selected = 1;
        assert!(ws.normalize_visible_selection(0));
        assert_eq!(ws.projects[0].selected, 2);

        ws.projects[0].sessions.push(test_session(4, None, ""));
        ws.projects[0].selected = 3;
        assert!(ws.normalize_visible_selection(0));
        assert_eq!(ws.projects[0].selected, 2);
    }

    #[test]
    fn move_selected_visible_session_skips_hidden_when_current_is_hidden() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ws = Workspace::discover(vec![tmp.path().to_path_buf()]);
        ws.projects[0].sessions = vec![
            test_session(1, Some(tmp.path().join("a.jsonl")), "a"),
            test_session(2, None, ""),
            test_session(3, Some(tmp.path().join("c.jsonl")), "c"),
        ];

        ws.projects[0].selected = 1;
        assert!(ws.move_selected_visible_session(0, 1));
        assert_eq!(ws.projects[0].selected, 2);

        ws.projects[0].selected = 1;
        assert!(ws.move_selected_visible_session(0, -1));
        assert_eq!(ws.projects[0].selected, 0);
    }

    #[test]
    fn move_selected_visible_session_wraps_across_visible_sessions_only() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ws = Workspace::discover(vec![tmp.path().to_path_buf()]);
        ws.projects[0].sessions = vec![
            test_session(1, Some(tmp.path().join("a.jsonl")), "a"),
            test_session(2, None, ""),
            test_session(3, Some(tmp.path().join("c.jsonl")), "c"),
        ];

        ws.projects[0].selected = 2;
        assert!(ws.move_selected_visible_session(0, 1));
        assert_eq!(ws.projects[0].selected, 0);

        ws.projects[0].selected = 0;
        assert!(ws.move_selected_visible_session(0, -1));
        assert_eq!(ws.projects[0].selected, 2);
    }

    #[test]
    fn normalize_visible_selection_is_noop_when_no_visible_sessions_exist() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ws = Workspace::discover(vec![tmp.path().to_path_buf()]);
        ws.projects[0].sessions = vec![test_session(1, None, "")];
        ws.projects[0].selected = 0;
        assert!(!ws.normalize_visible_selection(0));
        assert_eq!(ws.projects[0].selected, 0);
        assert!(!ws.move_selected_visible_session(0, 1));
    }

    #[test]
    fn ensure_project_session_creates_draft_for_empty_project() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ws = Workspace::discover(vec![tmp.path().to_path_buf()]);
        assert!(ws.projects[0].sessions.is_empty());
        ws.ensure_project_session(0).unwrap();
        assert_eq!(ws.projects[0].sessions.len(), 1);
        assert!(ws.projects[0].sessions[0].is_draft());
        assert!(ws.projects[0].sessions[0].is_unmaterialized_draft());
    }

    #[test]
    fn add_project_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ws = Workspace::discover(vec![]);
        let a = ws.add_project(tmp.path().to_path_buf());
        let b = ws.add_project(tmp.path().to_path_buf());
        assert_eq!(a, b);
        assert_eq!(ws.projects.len(), 1);
        ws.remove_project(0);
        assert!(ws.projects.is_empty());
    }
}
