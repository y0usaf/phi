use std::path::PathBuf;

use crate::extension::LuaExtension;

pub fn load_extensions(paths: &[PathBuf]) -> Vec<Box<dyn phi_ext::Extension>> {
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
