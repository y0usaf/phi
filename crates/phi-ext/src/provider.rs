use serde::{Deserialize, Serialize};

/// Configuration for registering a provider via `register_provider`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub name: String,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub api: Option<String>,
    #[serde(default)]
    pub models: Vec<ProviderModelConfig>,
}

/// A model configuration within a provider.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderModelConfig {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub context_window: u64,
    #[serde(default)]
    pub max_tokens: u64,
}
