//! Session archive: move session JSONL files into (and back out of) the
//! store-level `ARCHIVE` directory. Pure persistence over the session
//! store layout — frontends (mux, overlays) consume this; none of them
//! own the file format.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context as _, Result};

use crate::session::{self, SessionStore};

/// An archived session file, as listed for archive browsers.
#[derive(Clone, Debug)]
pub struct ArchivedSession {
    pub path: PathBuf,
    pub cwd: PathBuf,
    pub first_message: String,
    pub modified: Option<SystemTime>,
    /// Aggregated token/cost totals for the archived session.
    pub usage: crate::usage::UsageTotals,
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
                usage: crate::usage::session_usage_totals(&path),
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
    use crate::session::Role;

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
}
