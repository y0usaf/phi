//! Extension file discovery. Project-local `.phi/extensions/` is only
//! scanned when `include_project` is true — callers gate it on project
//! trust so a cloned repo's Lua never loads unprompted.

use std::path::{Path, PathBuf};

/// Global extensions directory: `<config>/phi/extensions`.
pub fn global_extensions_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "phi").map(|dirs| dirs.config_dir().join("extensions"))
}

/// Project-local extensions directory: `<cwd>/.phi/extensions`.
pub fn project_extensions_dir(cwd: &Path) -> PathBuf {
    cwd.join(".phi/extensions")
}

/// Whether the project has any `.lua` extensions (drives the trust prompt).
pub fn project_extensions_present(cwd: &Path) -> bool {
    !lua_files(&project_extensions_dir(cwd)).is_empty()
}

/// Discover extension files in load order: global first, then project
/// (each directory sorted by filename). Directories are explicit so tests
/// and embedded callers control the search space.
pub fn discover_extensions(
    global_dir: Option<&Path>,
    cwd: &Path,
    include_project: bool,
) -> Vec<PathBuf> {
    let mut results = Vec::new();
    if let Some(dir) = global_dir {
        results.extend(lua_files(dir));
    }
    if include_project {
        results.extend(lua_files(&project_extensions_dir(cwd)));
    }
    results
}

fn lua_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = read
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            (path.is_file() && path.extension().is_some_and(|e| e == "lua")).then_some(path)
        })
        .collect();
    files.sort();
    files
}
