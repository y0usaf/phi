//! Trust store for project-local extensions. Trust gates *extension
//! loading* only (DESIGN.md locked decision — no sandboxing in core). The
//! store is headless: deciding is the caller's job (a frontend prompts, a
//! test writes directly); the host only remembers.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustDecision {
    Trusted,
    Denied,
    Undecided,
}

/// Persistent map of canonicalized project dir → trusted flag, stored as
/// JSON at [`TrustStore::default_path`] (or any explicit path).
#[derive(Debug)]
pub struct TrustStore {
    path: PathBuf,
    entries: BTreeMap<String, bool>,
}

impl TrustStore {
    /// Default location: `<config>/phi/trust.json`.
    pub fn default_path() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "phi")
            .map(|dirs| dirs.config_dir().join("trust.json"))
    }

    /// Open a store; a missing or unreadable file is an empty store.
    pub fn open(path: PathBuf) -> Self {
        let entries = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self { path, entries }
    }

    pub fn decision(&self, dir: &Path) -> TrustDecision {
        match self.entries.get(&store_key(dir)) {
            Some(true) => TrustDecision::Trusted,
            Some(false) => TrustDecision::Denied,
            None => TrustDecision::Undecided,
        }
    }

    /// Record a decision and persist it.
    pub fn set(&mut self, dir: &Path, trusted: bool) -> Result<()> {
        self.entries.insert(store_key(dir), trusted);
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(&self.entries)?;
        std::fs::write(&self.path, json)
            .with_context(|| format!("writing {}", self.path.display()))?;
        Ok(())
    }
}

fn store_key(dir: &Path) -> String {
    dir.canonicalize()
        .unwrap_or_else(|_| dir.to_path_buf())
        .display()
        .to_string()
}
