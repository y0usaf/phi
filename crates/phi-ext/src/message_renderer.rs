use std::sync::Arc;

use serde_json::Value;

/// A custom message renderer registered by an extension.
pub struct MessageRendererSpec {
    pub custom_type: String,
    pub handler: Arc<dyn Fn(Value) -> anyhow::Result<String> + Send + Sync>,
}
