use std::path::{Path, PathBuf};

pub fn discover_extensions(cwd: &Path) -> Vec<PathBuf> {
    let mut results = Vec::new();

    // 1. Global config dir: <config>/phi/extensions/*.lua
    if let Some(config_dir) = dirs::config_dir() {
        let global_ext_dir = config_dir.join("phi/extensions");
        collect_lua_files(&global_ext_dir, &mut results);
    }

    // 2. Local cwd dir: <cwd>/.phi/extensions/*.lua
    let local_ext_dir = cwd.join(".phi/extensions");
    collect_lua_files(&local_ext_dir, &mut results);

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
            if path.is_file() && path.extension().map(|e| e == "lua").unwrap_or(false) {
                Some(path)
            } else {
                None
            }
        })
        .collect();

    files.sort();
    out.extend(files);
}
