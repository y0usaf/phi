use std::collections::BTreeMap;

use anyhow::{Context as _, Result};
use phi_ai::types::{Api, CostRates, Modality, Model, ThinkingLevel};
use serde::Deserialize;

use super::common::fetch_capped_json;
use super::models_dev::{adaptive_xhigh_effort, is_anthropic_adaptive};

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

/// Compute thinking levels and adaptive flag for a Vercel AI Gateway model.
///
/// All AI Gateway models use the Anthropic Messages API. For reasoning models:
/// - **Adaptive thinking** (Opus 4.6+, Sonnet 4.6): effort-based levels via
///   `output_config.effort`.
/// - **Budget-based thinking** (all other reasoning models, e.g. GLM): token
///   budget levels via `thinking.budget_tokens`. The gateway API doesn't expose
///   `reasoning_options` metadata, so we use the same default budget table as
///   `models_dev.rs` does for non-adaptive Anthropic Messages models.
///
/// Non-reasoning models get `None`.
fn gateway_thinking(
    model_id: &str,
    reasoning: bool,
    max_tokens: u64,
) -> (Option<BTreeMap<ThinkingLevel, Option<String>>>, bool) {
    if !reasoning {
        return (None, false);
    }
    if is_anthropic_adaptive(model_id) {
        let mut map: BTreeMap<_, _> = [
            (ThinkingLevel::Minimal, "low"),
            (ThinkingLevel::Low, "low"),
            (ThinkingLevel::Medium, "medium"),
            (ThinkingLevel::High, "high"),
        ]
        .iter()
        .map(|(l, e)| (*l, Some((*e).to_owned())))
        .collect();
        map.insert(
            ThinkingLevel::XHigh,
            Some(adaptive_xhigh_effort(model_id).to_owned()),
        );
        (Some(map), true)
    } else {
        // Budget-based thinking for non-adaptive reasoning models.
        let map: BTreeMap<_, _> = ANTHROPIC_BUDGETS
            .iter()
            .filter(|(_, budget)| *budget < max_tokens)
            .map(|(level, budget)| (*level, Some(budget.to_string())))
            .collect();
        if map.is_empty() {
            (None, false)
        } else {
            (Some(map), false)
        }
    }
}

/// Default token budgets for budget-based thinking on the Anthropic Messages
/// API. Matches the `ANTHROPIC_BUDGETS` table in `models_dev.rs`.
const ANTHROPIC_BUDGETS: &[(ThinkingLevel, u64)] = &[
    (ThinkingLevel::Minimal, 1024),
    (ThinkingLevel::Low, 4096),
    (ThinkingLevel::Medium, 8192),
    (ThinkingLevel::High, 16384),
    (ThinkingLevel::XHigh, 32768),
];

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
            let reasoning = m.tags.iter().any(|tag| tag == "reasoning");
            let max_tokens = m.max_tokens.unwrap_or(4096);
            let (thinking_levels, adaptive_thinking) = gateway_thinking(&m.id, reasoning, max_tokens);
            models.push(Model {
                id: m.id.clone(),
                name: m.name.unwrap_or(m.id),
                api: Api::AnthropicMessages,
                provider: "vercel-ai-gateway".to_owned(),
                base_url: AI_GATEWAY_BASE_URL.to_owned(),
                reasoning,
                input,
                cost: CostRates {
                    input: m.pricing.input * 1e6,
                    output: m.pricing.output * 1e6,
                    cache_read: m.pricing.input_cache_read * 1e6,
                    cache_write: m.pricing.input_cache_write * 1e6,
                },
                context_window: m.context_window.unwrap_or(4096),
                max_tokens,
                headers: BTreeMap::new(),
                thinking_levels,
                adaptive_thinking,
            });
        }
        Ok(models)
    })
}
