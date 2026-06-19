pub mod discovery;
pub mod extension;
pub use discovery::discover_extensions;
pub use extension::LuaExtension;

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

pub fn load_extensions(paths: &[std::path::PathBuf]) -> Vec<Box<dyn phi_ext::Extension>> {
    paths
        .iter()
        .filter_map(|path| match LuaExtension::from_file(path) {
            Ok(ext) => Some(Box::new(ext) as Box<dyn phi_ext::Extension>),
            Err(e) => {
                eprintln!("[phi-lua] skipping {}: {e}", path.display());
                None
            }
        })
        .collect()
}
