use std::collections::BTreeMap;

use anyhow::{Context as _, Result};
use phi_ai::types::{Api, CostRates, Modality, Model};
use serde::Deserialize;

use super::common::fetch_capped_json;

const AI_GATEWAY_MODELS_URL: &str = "https://ai-gateway.vercel.sh/v1/models";
const AI_GATEWAY_BASE_URL: &str = "https://ai-gateway.vercel.sh";

#[derive(Deserialize, Default)]
struct AiGatewayResponse {
    #[serde(default)]
    data: Vec<AiGatewayModel>,
}

#[derive(Deserialize)]
struct AiGatewayModel {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    context_window: Option<u64>,
    #[serde(default)]
    max_tokens: Option<u64>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    pricing: AiGatewayPricing,
}

#[derive(Deserialize, Default)]
struct AiGatewayPricing {
    #[serde(default, deserialize_with = "de_number")]
    input: f64,
    #[serde(default, deserialize_with = "de_number")]
    output: f64,
    #[serde(default, deserialize_with = "de_number")]
    input_cache_read: f64,
    #[serde(default, deserialize_with = "de_number")]
    input_cache_write: f64,
}

fn de_number<'de, D>(deserializer: D) -> std::result::Result<f64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize as _;
    let value = serde_json::Value::deserialize(deserializer)?;
    let parsed = match value {
        serde_json::Value::Number(n) => n.as_f64().unwrap_or(0.0),
        serde_json::Value::String(s) => s.parse::<f64>().unwrap_or(0.0),
        serde_json::Value::Null => 0.0,
        _ => 0.0,
    };
    Ok(if parsed.is_finite() { parsed } else { 0.0 })
}

pub(super) fn fetch_ai_gateway_models(runtime: &tokio::runtime::Runtime) -> Result<Vec<Model>> {
    runtime.block_on(async {
        eprintln!("fetching {AI_GATEWAY_MODELS_URL}...");
        let response: AiGatewayResponse = fetch_capped_json(AI_GATEWAY_MODELS_URL)
            .await
            .context("fetching Vercel AI Gateway models")?;

        let mut models = Vec::new();
        for m in response.data {
            if !m.tags.iter().any(|tag| tag == "tool-use") {
                continue;
            }
            let mut input = vec![Modality::Text];
            if m.tags.iter().any(|tag| tag == "vision") {
                input.push(Modality::Image);
            }
            models.push(Model {
                id: m.id.clone(),
                name: m.name.unwrap_or(m.id),
                api: Api::AnthropicMessages,
                provider: "vercel-ai-gateway".to_owned(),
                base_url: AI_GATEWAY_BASE_URL.to_owned(),
                reasoning: m.tags.iter().any(|tag| tag == "reasoning"),
                input,
                cost: CostRates {
                    input: m.pricing.input * 1e6,
                    output: m.pricing.output * 1e6,
                    cache_read: m.pricing.input_cache_read * 1e6,
                    cache_write: m.pricing.input_cache_write * 1e6,
                },
                context_window: m.context_window.unwrap_or(4096),
                max_tokens: m.max_tokens.unwrap_or(4096),
                headers: BTreeMap::new(),
                thinking_levels: None,
            });
        }
        Ok(models)
    })
}
