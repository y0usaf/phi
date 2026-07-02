//! Project trust persistence. A project directory containing `.phi/`
//! extensions, config, skills, or prompt overrides gets nothing loaded until
//! the user has explicitly trusted it (pi's stance: a cloned repo must not
//! execute code just by being opened). Decisions are persisted per canonical
//! project path.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

/// Does `cwd` contain project-local `.phi/` content that trust gates?
pub fn has_project_content(cwd: &Path) -> bool {
    let phi = cwd.join(".phi");
    if !phi.is_dir() {
        return false;
    }
    if phi.join("config.lua").is_file()
        || phi.join("SYSTEM.md").is_file()
        || phi.join("APPEND_SYSTEM.md").is_file()
        || phi.join("skills").is_dir()
    {
        return true;
    }
    // extensions/ counts only when it actually holds Lua files.
    std::fs::read_dir(phi.join("extensions"))
        .map(|entries| {
            entries
                .flatten()
                .any(|e| e.path().extension().is_some_and(|x| x == "lua"))
        })
        .unwrap_or(false)
}

/// Persisted per-project trust decisions, keyed by canonical path.
pub struct TrustStore {
    path: PathBuf,
    entries: BTreeMap<String, bool>,
}

impl TrustStore {
    /// The default store location: `<data-dir>/phi/trust.json`.
    pub fn load_default() -> Self {
        let path = directories::ProjectDirs::from("", "", "phi")
            .map(|dirs| dirs.data_dir().join("trust.json"))
            .unwrap_or_else(|| PathBuf::from("trust.json"));
        Self::load(path)
    }

    /// Load from `path`; a missing or unreadable file is an empty store
    /// (no decisions recorded), never an error.
    pub fn load(path: PathBuf) -> Self {
        let entries = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self { path, entries }
    }

    fn key(cwd: &Path) -> String {
        cwd.canonicalize()
            .unwrap_or_else(|_| cwd.to_path_buf())
            .display()
            .to_string()
    }

    /// The recorded decision for `cwd`, if any.
    pub fn decision(&self, cwd: &Path) -> Option<bool> {
        self.entries.get(&Self::key(cwd)).copied()
    }

    /// Record and persist a decision for `cwd`.
    pub fn set(&mut self, cwd: &Path, trusted: bool) -> Result<()> {
        self.entries.insert(Self::key(cwd), trusted);
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(&self.entries)?;
        std::fs::write(&self.path, json)
            .with_context(|| format!("writing trust store {}", self.path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn empty_dir_has_no_project_content() {
        let dir = tempdir().unwrap();
        assert!(!has_project_content(dir.path()));
        // An empty .phi/ (e.g. only sessions) is not gated content.
        std::fs::create_dir_all(dir.path().join(".phi/extensions")).unwrap();
        assert!(!has_project_content(dir.path()));
    }

    #[test]
    fn lua_extensions_and_overrides_are_gated_content() {
        let dir = tempdir().unwrap();
        let ext = dir.path().join(".phi/extensions");
        std::fs::create_dir_all(&ext).unwrap();
        std::fs::write(ext.join("evil.lua"), "-- hi").unwrap();
        assert!(has_project_content(dir.path()));

        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".phi")).unwrap();
        std::fs::write(dir.path().join(".phi/SYSTEM.md"), "override").unwrap();
        assert!(has_project_content(dir.path()));
    }

    #[test]
    fn decisions_persist_across_loads() {
        let dir = tempdir().unwrap();
        let store_path = dir.path().join("trust.json");
        let project = dir.path().join("project");
        std::fs::create_dir_all(&project).unwrap();

        let mut store = TrustStore::load(store_path.clone());
        assert_eq!(store.decision(&project), None);
        store.set(&project, true).unwrap();

        let store = TrustStore::load(store_path.clone());
        assert_eq!(store.decision(&project), Some(true));

        let mut store = TrustStore::load(store_path.clone());
        store.set(&project, false).unwrap();
        let store = TrustStore::load(store_path);
        assert_eq!(store.decision(&project), Some(false));
    }
}
