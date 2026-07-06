//! Active-agent persistence (WS1). `/agent <name>` switches the agent
//! profile at runtime; the choice is persisted here (user-global — a
//! look-and-feel of the agent, not a per-project decision) so it survives
//! restarts. A persisted choice wins over the declarative `agent` field in
//! config.lua, the same layering as the style store.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
struct AgentState {
    active: Option<String>,
}

/// The persisted agent choice.
pub struct AgentStore {
    path: PathBuf,
    state: AgentState,
}

impl AgentStore {
    /// The default store location: `<data-dir>/phi/agent.json`.
    pub fn load_default() -> Self {
        let path = directories::ProjectDirs::from("", "", "phi")
            .map(|dirs| dirs.data_dir().join("agent.json"))
            .unwrap_or_else(|| PathBuf::from("agent.json"));
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

    /// The persisted active agent name, if one was recorded.
    pub fn active(&self) -> Option<&str> {
        self.state.active.as_deref()
    }

    /// Record and persist the active agent (`None` clears the choice).
    pub fn set(&mut self, name: Option<&str>) -> Result<()> {
        self.state.active = name.map(str::to_owned);
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(&self.state)?;
        std::fs::write(&self.path, json)
            .with_context(|| format!("writing agent store {}", self.path.display()))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Resolve the agent to activate at runtime boot: the persisted `/agent`
/// choice wins over the declarative config.lua `agent` value. `None` means
/// no agent — the engine defaults apply.
pub fn initial_agent(config_agent: Option<String>) -> Option<String> {
    AgentStore::load_default()
        .active()
        .map(str::to_owned)
        .or(config_agent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn choice_persists_across_loads() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("agent.json");

        let mut store = AgentStore::load(path.clone());
        assert_eq!(store.active(), None);
        store.set(Some("pi")).unwrap();

        let store = AgentStore::load(path.clone());
        assert_eq!(store.active(), Some("pi"));

        let mut store = AgentStore::load(path.clone());
        store.set(None).unwrap();
        let store = AgentStore::load(path);
        assert_eq!(store.active(), None);
    }

    #[test]
    fn unreadable_file_is_an_empty_store() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("agent.json");
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(AgentStore::load(path).active(), None);
    }
}
