use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct MemoryConfig {
    pub enabled: bool,
    pub vault_path: PathBuf,
    pub recall_limit: usize,
    pub auto_inject: bool,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            vault_path: dirs::data_local_dir()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join("phi")
                .join("memory.db"),
            recall_limit: 5,
            auto_inject: true,
        }
    }
}
