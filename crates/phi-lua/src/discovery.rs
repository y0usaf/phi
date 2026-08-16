use std::path::{Path, PathBuf};

/// Discover Lua extension files. Project-local `.phi/extensions/` is only
/// scanned when `include_project` is true — callers gate it on project trust
/// so a cloned repo's Lua never loads unprompted.
pub fn discover_extensions(cwd: &Path, include_project: bool) -> Vec<PathBuf> {
    let mut results = Vec::new();

    // 1. Global config dir: <config>/phi/extensions/*.lua
    if let Some(config_dir) = crate::config::global_config_dir() {
        let global_ext_dir = config_dir.join("extensions");
        collect_lua_files(&global_ext_dir, &mut results);
    }

    // 2. Local cwd dir: <cwd>/.phi/extensions/*.lua
    if include_project {
        let local_ext_dir = cwd.join(".phi/extensions");
        collect_lua_files(&local_ext_dir, &mut results);
    }

    results
}

fn collect_lua_files(dir: &Path, out: &mut Vec<PathBuf>) {
    if !dir.is_dir() {
        return;
    }

    let read = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return,
    };

    let mut files: Vec<PathBuf> = read
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            if path.is_file() && path.extension().is_some_and(|e| e == "lua") {
                Some(path)
            } else {
                None
            }
        })
        .collect();

    files.sort();
    out.extend(files);
}
