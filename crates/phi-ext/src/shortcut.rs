use std::sync::Arc;

use crate::ExtensionContextRef;

/// A keyboard shortcut registered by an extension.
pub struct ShortcutSpec {
    pub key: String,
    pub description: String,
    pub handler: Arc<dyn Fn(ExtensionContextRef) -> anyhow::Result<()> + Send + Sync>,
}
