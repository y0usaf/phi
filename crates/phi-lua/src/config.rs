pub fn config_extension_paths(cwd: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut paths = Vec::new();
    // Global config: same path as Config::load() uses
    if let Some(proj_dirs) = directories::ProjectDirs::from("", "", "phi") {
        let p = proj_dirs.config_dir().join("config.lua");
        if p.exists() {
            paths.push(p);
        }
    }
    // Project config
    let p = cwd.join(".phi").join("config.lua");
    if p.exists() {
        paths.push(p);
    }
    paths
}
