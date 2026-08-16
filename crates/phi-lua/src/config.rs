/// The global config directory (the `config_dir()` of the `phi` project dirs).
/// Shared by config and discovery so the trust gating around project files is
/// stated in one place. Returns `None` when the platform has no such directory.
pub fn global_config_dir() -> Option<std::path::PathBuf> {
    directories::ProjectDirs::from("", "", "phi").map(|d| d.config_dir().to_path_buf())
}

/// Paths of `config.lua` files to load as extensions. The project-local one
/// is only included when `include_project` is true — callers gate it on
/// project trust.
pub fn config_extension_paths(
    cwd: &std::path::Path,
    include_project: bool,
) -> Vec<std::path::PathBuf> {
    let mut paths = Vec::new();
    // Global config: same path as Config::load() uses
    if let Some(config_dir) = global_config_dir() {
        let p = config_dir.join("config.lua");
        if p.exists() {
            paths.push(p);
        }
    }
    // Project config
    if include_project {
        let p = cwd.join(".phi").join("config.lua");
        if p.exists() {
            paths.push(p);
        }
    }
    paths
}
