//! Active-style persistence (WS10). `/style <name>` switches the visual
//! style pack at runtime; the choice is persisted here (user-global — a
//! look-and-feel, not a per-project decision) so it survives restarts. A
//! persisted choice wins over the declarative `style` field in config.lua,
//! the same layering as the approvals extension's saved answers.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
struct StyleState {
    active: Option<String>,
}

/// The persisted style choice.
pub struct StyleStore {
    path: PathBuf,
    state: StyleState,
}

impl StyleStore {
    /// The default store location: `<data-dir>/phi/style.json`.
    pub fn load_default() -> Self {
        let path = directories::ProjectDirs::from("", "", "phi")
            .map(|dirs| dirs.data_dir().join("style.json"))
            .unwrap_or_else(|| PathBuf::from("style.json"));
        Self::load(path)
    }

    /// Load from `path`; a missing or unreadable file is an empty store
    /// (no choice recorded), never an error.
    pub fn load(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let state = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self { path, state }
    }

    /// The persisted active style name, if one was recorded.
    pub fn active(&self) -> Option<&str> {
        self.state.active.as_deref()
    }

    /// Record and persist the active style (`None` clears the choice).
    pub fn set(&mut self, name: Option<&str>) -> Result<()> {
        self.state.active = name.map(str::to_owned);
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(&self.state)?;
        std::fs::write(&self.path, json)
            .with_context(|| format!("writing style store {}", self.path.display()))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Resolve the style to activate at runtime boot: the persisted `/style`
/// choice wins over the declarative config.lua `style` value. `None` means
/// no style — the base registrations apply.
pub fn initial_style(config_style: Option<String>) -> Option<String> {
    StyleStore::load_default()
        .active()
        .map(str::to_owned)
        .or(config_style)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn choice_persists_across_loads() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("style.json");

        let mut store = StyleStore::load(path.clone());
        assert_eq!(store.active(), None);
        store.set(Some("crush")).unwrap();

        let store = StyleStore::load(path.clone());
        assert_eq!(store.active(), Some("crush"));

        let mut store = StyleStore::load(path.clone());
        store.set(None).unwrap();
        let store = StyleStore::load(path);
        assert_eq!(store.active(), None);
    }

    #[test]
    fn unreadable_file_is_an_empty_store() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("style.json");
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(StyleStore::load(path).active(), None);
    }
}
