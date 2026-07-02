//! Mux workspace: the projects and sessions shown in the sidebar, plus
//! archive/restore of session files. Sessions are discovered from the
//! on-disk session store; embedded handles are attached lazily when a
//! session is first focused.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context as _, Result};

use crate::driver::{SessionHandle, SessionKey};
use crate::status::SessionStatus;
use phi_session::session::{self, SessionStore};

pub struct Workspace {
    pub projects: Vec<MuxProject>,
    next_key: SessionKey,
}

pub struct MuxProject {
    pub cwd: PathBuf,
    pub sessions: Vec<MuxSession>,
}

/// A view's selection state: which project is focused and, per project,
/// which session. Lives in the view rather than the workspace (WS8.2) so
/// each attached client can navigate independently while the workspace
/// stays canonical.
#[derive(Clone, Debug, Default)]
pub struct Focus {
    /// Index of the focused project in [`Workspace::projects`].
    pub project: usize,
    /// Selected session index per project, parallel to
    /// [`Workspace::projects`]. Missing entries mean 0.
    selected: Vec<usize>,
}

/// The focused session captured (by key) before a project rebuild, so the
/// selection can be re-resolved after indices shift. See [`Focus::restore`].
pub struct FocusMark {
    key: Option<SessionKey>,
    index: usize,
}

impl Focus {
    pub fn selected_session(&self, project: usize) -> usize {
        self.selected.get(project).copied().unwrap_or(0)
    }

    pub fn set_selected_session(&mut self, project: usize, session: usize) {
        if self.selected.len() <= project {
            self.selected.resize(project + 1, 0);
        }
        self.selected[project] = session;
    }

    /// Clamp the selected session into the project's bounds.
    pub fn clamp_session(&mut self, ws: &Workspace, project: usize) {
        let len = ws.projects.get(project).map_or(0, |p| p.sessions.len());
        let selected = self.selected_session(project);
        let clamped = selected.min(len.saturating_sub(1));
        if clamped != selected {
            self.set_selected_session(project, clamped);
        }
    }

    /// Forget the removed project's selection (entries after it shift down
    /// with the projects vec) and keep the focused project in bounds.
    pub fn project_removed(&mut self, index: usize, remaining: usize) {
        if index < self.selected.len() {
            self.selected.remove(index);
        }
        if self.project >= remaining {
            self.project = remaining.saturating_sub(1);
        }
    }

    /// Capture the focused session before a project rebuild. Call
    /// [`Workspace::stabilize_keys`] first so an unmaterialized draft has a
    /// real key to be found by afterwards.
    pub fn mark(&self, ws: &Workspace, project: usize) -> FocusMark {
        let index = self.selected_session(project);
        let key = ws
            .projects
            .get(project)
            .and_then(|p| p.sessions.get(index))
            .map(|s| s.key);
        FocusMark { key, index }
    }

    /// Re-resolve a marked selection after a rebuild: look the session up by
    /// key first (handles reordering from the newest-first sort). If it was
    /// removed (archived/deleted by another process), fall back to the
    /// original index clamped to bounds rather than jumping to index 0 — the
    /// newest session — which would steal focus from whatever the user was
    /// looking at.
    pub fn restore(&mut self, ws: &Workspace, project: usize, mark: FocusMark) {
        let Some(p) = ws.projects.get(project) else {
            return;
        };
        let next = mark
            .key
            .and_then(|key| p.sessions.iter().position(|s| s.key == key))
            .unwrap_or(mark.index.min(p.sessions.len().saturating_sub(1)));
        self.set_selected_session(project, next);
    }

    /// If the selected session is hidden (an unmaterialized draft), move the
    /// selection to the next visible session, falling back to the previous
    /// one. Returns whether the selection changed.
    pub fn normalize_visible(&mut self, ws: &Workspace, project_index: usize) -> bool {
        let Some(project) = ws.projects.get(project_index) else {
            return false;
        };
        if project.sessions.is_empty() {
            self.set_selected_session(project_index, 0);
            return false;
        }
        self.clamp_session(ws, project_index);
        let current = self.selected_session(project_index);
        if project
            .sessions
            .get(current)
            .is_some_and(MuxSession::is_sidebar_visible)
        {
            return false;
        }

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
                let changed = index != current;
                self.set_selected_session(project_index, index);
                changed
            }
            None => false,
        }
    }

    /// Move the selection through the project's visible sessions by `delta`,
    /// wrapping. Returns whether the selection changed.
    pub fn move_visible(&mut self, ws: &Workspace, project_index: usize, delta: i32) -> bool {
        if delta == 0 {
            return false;
        }
        let Some(project) = ws.projects.get(project_index) else {
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

        let selected = self.selected_session(project_index);
        let current = selected.min(project.sessions.len().saturating_sub(1));
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
        let changed = next != selected;
        self.set_selected_session(project_index, next);
        changed
    }
}

/// Rescan a project while preserving a view's selection across the rebuild.
pub fn refresh_project_with_focus(
    ws: &mut Workspace,
    focus: &mut Focus,
    index: usize,
) -> RefreshDelta {
    refresh_project_with_focuses(ws, &mut [focus], index)
}

/// Rescan a project while preserving every attached view's selection across
/// the rebuild: one [`Focus::mark`]/[`Focus::restore`] pair per view.
pub fn refresh_project_with_focuses(
    ws: &mut Workspace,
    focuses: &mut [&mut Focus],
    index: usize,
) -> RefreshDelta {
    ws.stabilize_keys(index);
    let marks: Vec<FocusMark> = focuses.iter().map(|focus| focus.mark(ws, index)).collect();
    let delta = ws.refresh_project(index);
    for (focus, mark) in focuses.iter_mut().zip(marks) {
        focus.restore(ws, index, mark);
    }
    delta
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

/// Sessions whose backing files vanished during a refresh (archived or
/// deleted by another process).  `titles` is populated for sessions that had
/// a visible sidebar entry so the caller can surface a status-bar note.
#[derive(Clone, Debug, Default)]
pub struct RefreshDelta {
    /// Sidebar titles of sessions dropped because their files no longer exist.
    pub archived: Vec<String>,
}

impl Workspace {
    fn empty() -> Self {
        Self {
            projects: Vec::new(),
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
        let mut project = MuxProject { cwd, sessions };
        for s in &mut project.sessions {
            s.key = self.next_key;
            self.next_key += 1;
        }
        self.projects.push(project);
        self.projects.len() - 1
    }

    /// Drop a project from the workspace (files untouched); running session
    /// tasks abort when their handles drop. Views must follow up with
    /// [`Focus::project_removed`] to keep their selection in bounds.
    pub fn remove_project(&mut self, index: usize) {
        if index < self.projects.len() {
            self.projects.remove(index);
        }
    }

    /// Assign real keys to any key-0 (unassigned) sessions so a view can
    /// track its selection by key across a rebuild. Without this, an
    /// unmaterialized draft's key is 0 when captured by [`Focus::mark`],
    /// then reassigned to a real key during the rebuild, making the
    /// post-rebuild key lookup fail and causing the index fallback to jump
    /// focus to the newest session — a focus steal triggered by any
    /// external file change.
    pub fn stabilize_keys(&mut self, index: usize) {
        let Some(project) = self.projects.get_mut(index) else {
            return;
        };
        for s in &mut project.sessions {
            if s.key == 0 {
                s.key = self.next_key;
                self.next_key += 1;
            }
        }
    }

    /// Rescan a project's session directory, keeping live handles, drafts,
    /// and statuses for sessions that still exist.  Returns the titles of
    /// sessions whose backing files vanished since the last scan (archived or
    /// deleted by another process) so the caller can surface a status-bar note.
    ///
    /// The rebuild reorders sessions (newest-first sort), so callers holding
    /// a [`Focus`] must go through [`refresh_project_with_focus`] to keep
    /// their selection pointing at the same session.
    pub fn refresh_project(&mut self, index: usize) -> RefreshDelta {
        let Some(project) = self.projects.get_mut(index) else {
            return RefreshDelta::default();
        };
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
        // Keep the key-0 unmaterialized draft (so `new_draft` can reuse it)
        // and sessions whose backing file still exists.  A session whose
        // file vanished (with or without a live handle) was archived or
        // deleted by another process — drop it and report its title so the
        // caller can notify the user via the status bar.
        let mut delta = RefreshDelta::default();
        fresh.extend(old.into_iter().filter(|s| {
            // Always keep the reusable unmaterialized draft.
            if s.is_unmaterialized_draft() {
                return true;
            }
            // A running draft with no store_path yet — keep it alive.
            if s.handle.is_some() && s.store_path.is_none() {
                return true;
            }
            // Keep sessions whose backing file still exists on disk.
            if s.store_path.as_ref().is_some_and(|p| p.exists()) {
                return true;
            }
            // File is gone (or never existed for a stale draft).  Report
            // sessions that had a visible sidebar entry so the user knows
            // why they disappeared.
            if !s.is_draft() && !s.first_message.trim().is_empty() {
                delta.archived.push(s.first_message.clone());
            }
            false
        }));
        for s in &mut fresh {
            if s.key == 0 {
                s.key = self.next_key;
                self.next_key += 1;
            }
        }
        project.sessions = fresh;
        delta
    }

    /// New draft session at the top of the project; reuses an existing
    /// untouched draft instead of stacking empties. Returns the draft's
    /// index so the caller can point its [`Focus`] at it.
    pub fn new_draft(&mut self, project_index: usize) -> Option<usize> {
        let key = self.next_key();
        let project = self.projects.get_mut(project_index)?;
        if let Some(i) = project
            .sessions
            .iter()
            .position(MuxSession::is_unmaterialized_draft)
        {
            return Some(i);
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
        Some(0)
    }

    /// Ensure the project has at least one (possibly draft) session.
    pub fn ensure_project_session(&mut self, project_index: usize) {
        if self
            .projects
            .get(project_index)
            .is_some_and(|p| p.sessions.is_empty())
        {
            self.new_draft(project_index);
        }
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

    /// Remove the session at `selected` if it is an unmaterialized draft.
    /// The caller re-clamps its [`Focus`] afterwards.
    pub fn discard_unmaterialized_draft(&mut self, project_index: usize, selected: usize) -> bool {
        let Some(project) = self.projects.get_mut(project_index) else {
            return false;
        };
        if !project
            .sessions
            .get(selected)
            .is_some_and(MuxSession::is_unmaterialized_draft)
        {
            return false;
        }
        project.sessions.remove(selected);
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
    /// Aggregated token/cost totals for the archived session.
    pub usage: phi_session::usage::UsageTotals,
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
                usage: phi_session::usage::session_usage_totals(&path),
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
        let delta = ws.refresh_project(0);
        assert!(
            ws.projects[0].sessions.is_empty(),
            "stale draft should be dropped on refresh"
        );
        // A draft with no handle has no sidebar title to report.
        assert!(delta.archived.is_empty());
    }

    #[test]
    fn refresh_project_drops_session_with_live_handle_but_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ws = Workspace::discover(vec![tmp.path().to_path_buf()]);
        // Simulate a session that another process archived: it still has a
        // store_path (now pointing at a non-existent file) and no handle. The
        // session should be dropped on refresh and its title reported.
        let gone = tmp.path().join("vanished.jsonl");
        ws.projects[0].sessions = vec![test_session(1, Some(gone), "my session")];
        assert!(!ws.projects[0].sessions[0].is_draft());

        let delta = ws.refresh_project(0);
        assert!(
            ws.projects[0].sessions.is_empty(),
            "session with missing file should be dropped on refresh"
        );
        assert_eq!(delta.archived, vec!["my session"]);
    }

    #[test]
    fn discard_unmaterialized_draft_removes_selected_draft() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ws = Workspace::discover(vec![tmp.path().to_path_buf()]);
        let draft = ws.new_draft(0).unwrap();
        assert!(ws.discard_unmaterialized_draft(0, draft));
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
        let mut focus = Focus::default();
        ws.projects[0].sessions = vec![
            test_session(1, Some(tmp.path().join("a.jsonl")), "a"),
            test_session(2, None, ""),
            test_session(3, Some(tmp.path().join("c.jsonl")), "c"),
        ];

        focus.set_selected_session(0, 1);
        assert!(focus.normalize_visible(&ws, 0));
        assert_eq!(focus.selected_session(0), 2);

        ws.projects[0].sessions.push(test_session(4, None, ""));
        focus.set_selected_session(0, 3);
        assert!(focus.normalize_visible(&ws, 0));
        assert_eq!(focus.selected_session(0), 2);
    }

    #[test]
    fn move_selected_visible_session_skips_hidden_when_current_is_hidden() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ws = Workspace::discover(vec![tmp.path().to_path_buf()]);
        let mut focus = Focus::default();
        ws.projects[0].sessions = vec![
            test_session(1, Some(tmp.path().join("a.jsonl")), "a"),
            test_session(2, None, ""),
            test_session(3, Some(tmp.path().join("c.jsonl")), "c"),
        ];

        focus.set_selected_session(0, 1);
        assert!(focus.move_visible(&ws, 0, 1));
        assert_eq!(focus.selected_session(0), 2);

        focus.set_selected_session(0, 1);
        assert!(focus.move_visible(&ws, 0, -1));
        assert_eq!(focus.selected_session(0), 0);
    }

    #[test]
    fn move_selected_visible_session_wraps_across_visible_sessions_only() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ws = Workspace::discover(vec![tmp.path().to_path_buf()]);
        let mut focus = Focus::default();
        ws.projects[0].sessions = vec![
            test_session(1, Some(tmp.path().join("a.jsonl")), "a"),
            test_session(2, None, ""),
            test_session(3, Some(tmp.path().join("c.jsonl")), "c"),
        ];

        focus.set_selected_session(0, 2);
        assert!(focus.move_visible(&ws, 0, 1));
        assert_eq!(focus.selected_session(0), 0);

        focus.set_selected_session(0, 0);
        assert!(focus.move_visible(&ws, 0, -1));
        assert_eq!(focus.selected_session(0), 2);
    }

    #[test]
    fn normalize_visible_selection_is_noop_when_no_visible_sessions_exist() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ws = Workspace::discover(vec![tmp.path().to_path_buf()]);
        let mut focus = Focus::default();
        ws.projects[0].sessions = vec![test_session(1, None, "")];
        assert!(!focus.normalize_visible(&ws, 0));
        assert_eq!(focus.selected_session(0), 0);
        assert!(!focus.move_visible(&ws, 0, 1));
    }

    #[test]
    fn ensure_project_session_creates_draft_for_empty_project() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ws = Workspace::discover(vec![tmp.path().to_path_buf()]);
        assert!(ws.projects[0].sessions.is_empty());
        ws.ensure_project_session(0);
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

    /// Regression: when the user is looking at an unmaterialized draft (which
    /// has the sentinel key 0), a background refresh (triggered by a
    /// filesystem watcher event from another Phi instance) must NOT steal
    /// focus by jumping to index 0 (the newest session).  The draft's key
    /// must be stabilised before the rebuild so the post-rebuild key lookup
    /// succeeds and the selection is preserved.
    #[test]
    fn refresh_project_preserves_selection_on_unmaterialized_draft() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ws = Workspace::discover(vec![tmp.path().to_path_buf()]);
        let mut focus = Focus::default();
        // A file-backed session whose file actually exists (survives refresh).
        let real_file = tmp.path().join("existing.jsonl");
        std::fs::write(&real_file, "dummy").unwrap();
        ws.projects[0].sessions = vec![
            test_session(5, Some(real_file), "real session"),
            // The unmaterialized draft the user is currently looking at.
            MuxSession {
                key: 0,
                store_path: None,
                first_message: String::new(),
                modified: None,
                status: SessionStatus::Idle,
                handle: None,
            },
        ];
        focus.set_selected_session(0, 1); // user is on the draft

        let _ = refresh_project_with_focus(&mut ws, &mut focus, 0);

        // The draft must survive with a real (non-zero) key.
        let draft_idx = ws.projects[0]
            .sessions
            .iter()
            .position(|s| s.is_unmaterialized_draft());
        assert!(draft_idx.is_some(), "draft should survive refresh");
        assert_ne!(
            ws.projects[0].sessions[draft_idx.unwrap()].key,
            0,
            "draft should have a real key after refresh, not sentinel 0"
        );
        // Focus must stay on the draft, not jump to the real session.
        assert_eq!(
            focus.selected_session(0),
            draft_idx.unwrap(),
            "selection should stay on the draft, not steal focus to another session"
        );
    }

    /// Regression: when the selected session is removed (archived/deleted by
    /// another process) during a background refresh, the selection should
    /// fall back to the *original index* clamped to bounds, not jump to
    /// index 0 (the newest session) — which would steal focus.
    #[test]
    fn refresh_project_fallback_uses_clamped_index_not_zero() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ws = Workspace::discover(vec![tmp.path().to_path_buf()]);
        let mut focus = Focus::default();
        // Two surviving sessions (files exist) + one that will be dropped
        // (file doesn't exist).  User is on the one that gets dropped.
        let file_a = tmp.path().join("a.jsonl");
        let file_b = tmp.path().join("b.jsonl");
        std::fs::write(&file_a, "x").unwrap();
        std::fs::write(&file_b, "x").unwrap();
        ws.projects[0].sessions = vec![
            test_session(1, Some(file_a), "a"),
            test_session(2, Some(tmp.path().join("gone.jsonl")), "gone"),
            test_session(3, Some(file_b), "b"),
        ];
        focus.set_selected_session(0, 1); // on "gone", which will be dropped

        let _ = refresh_project_with_focus(&mut ws, &mut focus, 0);

        // "gone" was dropped (2 sessions remain: a, b).  The selected key (2)
        // is no longer found, so the fallback should use the original index
        // (1) clamped to the new length — NOT 0.
        assert_eq!(ws.projects[0].sessions.len(), 2);
        assert_eq!(
            focus.selected_session(0),
            1,
            "fallback should stay at original index 1, not jump to 0"
        );
    }

    /// Removing a project shifts the per-project selections and clamps the
    /// focused project into bounds.
    #[test]
    fn focus_project_removed_shifts_selections_and_clamps() {
        let mut focus = Focus::default();
        focus.set_selected_session(0, 3);
        focus.set_selected_session(1, 5);
        focus.set_selected_session(2, 7);
        focus.project = 2;

        focus.project_removed(1, 2);

        assert_eq!(focus.selected_session(0), 3);
        assert_eq!(focus.selected_session(1), 7);
        assert_eq!(focus.project, 1, "focused project clamps into bounds");
    }
}
