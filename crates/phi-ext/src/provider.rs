use std::collections::BTreeMap;

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
///
/// Everything beyond `id`/`name` is optional: a hand-written extension can
/// register a model with two fields, while the bundled catalog (and any
/// extension that has the data) carries full fidelity — cost, thinking
/// levels, modalities — so the host doesn't have to degrade to defaults.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderModelConfig {
    pub id: String,
    /// Display name; empty means "use the id".
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub context_window: u64,
    #[serde(default)]
    pub max_tokens: u64,
    /// USD per million tokens; omitted means unknown (rendered as zero).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<ProviderModelCost>,
    /// Thinking level name (`off`..`xhigh`) → provider-specific value
    /// (a budget, an effort string, or null for provider default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_levels: Option<BTreeMap<String, Option<String>>>,
    /// Input modalities (`text`, `image`); omitted means text-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Vec<String>>,
    /// Extra HTTP headers sent with every request to this model.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub adaptive_thinking: bool,
}

/// Cost rates in USD per million tokens.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct ProviderModelCost {
    #[serde(default)]
    pub input: f64,
    #[serde(default)]
    pub output: f64,
    #[serde(default)]
    pub cache_read: f64,
    #[serde(default)]
    pub cache_write: f64,
}
