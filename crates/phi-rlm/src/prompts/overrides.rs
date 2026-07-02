//! Prompt-override file discovery, mirroring pi's
//! `discoverSystemPromptFile` / `discoverAppendSystemPromptFile`.

use std::path::Path;

/// Discover prompt-override files from disk.
///
/// Lookup order (first found wins; project-level takes precedence over global):
///   - `<cwd>/.phi/SYSTEM.md`          — overrides the identity section
///   - `~/.config/phi/SYSTEM.md`       — fallback identity override
///   - `<cwd>/.phi/APPEND_SYSTEM.md`   — content appended to the system prompt
///   - `~/.config/phi/APPEND_SYSTEM.md` — fallback append
///
/// Returns `(system_prompt, append_prompt)`. Project-local `.phi/` files are
/// only read when `include_project` is true — callers gate it on project
/// trust.
pub fn discover_prompt_overrides(
    cwd: &Path,
    include_project: bool,
) -> (Option<String>, Option<String>) {
    let local = |name: &str| include_project.then(|| discover_file(cwd, name)).flatten();
    let system = local("SYSTEM.md").or_else(|| discover_global_file("SYSTEM.md"));
    let append = local("APPEND_SYSTEM.md").or_else(|| discover_global_file("APPEND_SYSTEM.md"));
    (system, append)
}

fn discover_file(cwd: &Path, name: &str) -> Option<String> {
    let path = cwd.join(".phi").join(name);
    std::fs::read_to_string(&path).ok()
}

fn discover_global_file(name: &str) -> Option<String> {
    let config_dir = directories::BaseDirs::new().map(|b| b.config_dir().to_path_buf())?;
    let path = config_dir.join("phi").join(name);
    std::fs::read_to_string(&path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn no_files_returns_none() {
        let dir = tempdir().unwrap();
        let (sys, append) = discover_prompt_overrides(dir.path(), true);
        assert!(sys.is_none());
        assert!(append.is_none());
    }

    #[test]
    fn local_system_md_overrides_identity() {
        let dir = tempdir().unwrap();
        let phi_dir = dir.path().join(".phi");
        fs::create_dir_all(&phi_dir).unwrap();
        fs::write(phi_dir.join("SYSTEM.md"), "You are a custom agent.").unwrap();
        let (sys, append) = discover_prompt_overrides(dir.path(), true);
        assert_eq!(sys.as_deref(), Some("You are a custom agent."));
        assert!(append.is_none());
    }

    #[test]
    fn local_append_md_is_found() {
        let dir = tempdir().unwrap();
        let phi_dir = dir.path().join(".phi");
        fs::create_dir_all(&phi_dir).unwrap();
        fs::write(phi_dir.join("APPEND_SYSTEM.md"), "Extra rules.").unwrap();
        let (sys, append) = discover_prompt_overrides(dir.path(), true);
        assert!(sys.is_none());
        assert_eq!(append.as_deref(), Some("Extra rules."));
    }

    #[test]
    fn both_files_are_found() {
        let dir = tempdir().unwrap();
        let phi_dir = dir.path().join(".phi");
        fs::create_dir_all(&phi_dir).unwrap();
        fs::write(phi_dir.join("SYSTEM.md"), "Custom identity.").unwrap();
        fs::write(phi_dir.join("APPEND_SYSTEM.md"), "Extra rules.").unwrap();
        let (sys, append) = discover_prompt_overrides(dir.path(), true);
        assert_eq!(sys.as_deref(), Some("Custom identity."));
        assert_eq!(append.as_deref(), Some("Extra rules."));
    }
}
