use std::collections::BTreeMap;

use anyhow::Result;
use phi_ai::types::{Api, CostRates, Modality, Model, ThinkingLevel};
use serde::Deserialize;

use super::common::fetch_capped_json;

const MODELS_DEV_URL: &str = "https://models.dev/api.json";

/// Per-model fixes for upstream listings that disagree with the runtime
/// server. Sourced from `limit.context`/`limit.output` is generally right,
/// but some providers advertise limits larger than what they actually serve.
/// Entries are `(provider_id, model_id, context_window, max_tokens)`.
///
/// Known bad cases as of 2026-06: neuralwatt's `/v1/models` lists
/// `glm-5.2` with max_context_length = 1048560 but their vLLM serving
/// endpoint enforces `max_model_len = max_total_tokens = 200000`.
const OVERRIDES: &[(&str, &str, u64, u64)] = &[("neuralwatt", "glm-5.2", 200_000, 65_536)];

pub(super) fn override_for(provider: &str, model: &str) -> Option<(u64, u64)> {
    OVERRIDES
        .iter()
        .find(|(p, m, _, _)| *p == provider && *m == model)
        .map(|(_, _, ctx, max)| (*ctx, *max))
}

#[derive(Deserialize)]
pub(super) struct DevProvider {
    pub(super) id: String,
    #[serde(default)]
    npm: Option<String>,
    /// OpenAI-compatible base URL, present for most third-party providers.
    #[serde(default)]
    api: Option<String>,
    #[serde(default)]
    models: BTreeMap<String, DevModel>,
}

#[derive(Deserialize)]
struct DevModel {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    tool_call: bool,
    #[serde(default)]
    reasoning: bool,
    #[serde(default)]
    reasoning_options: Vec<ReasoningOption>,
    #[serde(default)]
    modalities: Option<DevModalities>,
    #[serde(default)]
    limit: Option<DevLimit>,
    #[serde(default)]
    cost: Option<DevCost>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ReasoningOption {
    // `values` may contain null (provider default / off).
    Effort {
        values: Vec<Option<String>>,
    },
    BudgetTokens {},
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
struct DevModalities {
    #[serde(default)]
    input: Vec<String>,
}

#[derive(Deserialize)]
struct DevLimit {
    #[serde(default)]
    context: Option<u64>,
    #[serde(default)]
    output: Option<u64>,
}

#[derive(Deserialize, Default)]
struct DevCost {
    #[serde(default)]
    input: f64,
    #[serde(default)]
    output: f64,
    #[serde(default)]
    cache_read: f64,
    #[serde(default)]
    cache_write: f64,
}

/// Base URLs for first-party providers whose models.dev entry has no `api`
/// field (the npm SDK encodes the URL instead).
fn builtin_base_url(provider_id: &str) -> Option<&'static str> {
    Some(match provider_id {
        "anthropic" => "https://api.anthropic.com",
        "openai" => "https://api.openai.com/v1",
        "google" => "https://generativelanguage.googleapis.com/v1beta",
        "amazon-bedrock" => "https://bedrock-runtime.us-east-1.amazonaws.com",
        "mistral" => "https://api.mistral.ai/v1",
        "groq" => "https://api.groq.com/openai/v1",
        "cerebras" => "https://api.cerebras.ai/v1",
        "xai" => "https://api.x.ai/v1",
        _ => return None,
    })
}

pub(super) fn classify(provider: &DevProvider) -> Option<(Api, String)> {
    let npm = provider.npm.as_deref().unwrap_or("");
    let api = match npm {
        "@ai-sdk/anthropic" => Api::AnthropicMessages,
        "@ai-sdk/openai" => Api::OpenAiResponses,
        "@ai-sdk/google" => Api::GoogleGenerativeAi,
        "@ai-sdk/amazon-bedrock" => Api::BedrockConverseStream,
        "@ai-sdk/mistral" => Api::MistralConversations,
        // Base URL depends on project/region/resource configuration; these
        // can't be represented as a static registry entry.
        "@ai-sdk/google-vertex" | "@ai-sdk/google-vertex/anthropic" | "@ai-sdk/azure" => {
            return None;
        }
        // Bespoke wire protocols we have no Api variant for.
        "@ai-sdk/cohere" => return None,
        // Everything else on models.dev speaks OpenAI chat completions.
        _ => Api::OpenAiCompletions,
    };
    let base_url = provider
        .api
        .as_deref()
        .or_else(|| builtin_base_url(&provider.id))?
        .trim_end_matches('/')
        .to_owned();
    Some((api, base_url))
}

const ANTHROPIC_BUDGETS: &[(ThinkingLevel, u64)] = &[
    (ThinkingLevel::Minimal, 1024),
    (ThinkingLevel::Low, 4096),
    (ThinkingLevel::Medium, 8192),
    (ThinkingLevel::High, 16384),
    (ThinkingLevel::XHigh, 32768),
];

/// Effort levels for Anthropic adaptive thinking models (Opus 4.6+, Sonnet
/// 4.6). These map thinking levels to effort strings that Anthropic's API
/// accepts via `output_config.effort` alongside `thinking.type: "adaptive"`.
const ANTHROPIC_ADAPTIVE_EFFORTS: &[(ThinkingLevel, &str)] = &[
    (ThinkingLevel::Minimal, "low"),
    (ThinkingLevel::Low, "low"),
    (ThinkingLevel::Medium, "medium"),
    (ThinkingLevel::High, "high"),
];

/// Detect Anthropic adaptive thinking models (Opus 4.6+, Sonnet 4.6+).
/// These models use `thinking.type: "adaptive"` + `output_config.effort`
/// instead of the older `thinking.type: "enabled"` + `budget_tokens`.
/// Matches Pi's `isAnthropicAdaptiveThinkingModel`.
pub(super) fn is_anthropic_adaptive(model_id: &str) -> bool {
    let id = model_id.to_ascii_lowercase();
    id.contains("opus-4-6")
        || id.contains("opus-4.6")
        || id.contains("opus-4-7")
        || id.contains("opus-4.7")
        || id.contains("opus-4-8")
        || id.contains("opus-4.8")
        || id.contains("sonnet-4-6")
        || id.contains("sonnet-4.6")
        || id.contains("sonnet-5")
        || id.contains("sonnet.5")
}

/// Determine the `xhigh` effort value for an adaptive model.
/// Opus 4.6 supports `max`; Opus 4.7/4.8, Sonnet 4.6, and Sonnet 5 support `xhigh`.
/// Matches Pi's thinkingLevelMap xhigh overrides.
pub(super) fn adaptive_xhigh_effort(model_id: &str) -> &'static str {
    let id = model_id.to_ascii_lowercase();
    if id.contains("opus-4-6") || id.contains("opus-4.6") {
        "max"
    } else {
        "xhigh"
    }
}

const EFFORT_LEVELS: &[(ThinkingLevel, &str)] = &[
    (ThinkingLevel::Off, "none"),
    (ThinkingLevel::Minimal, "minimal"),
    (ThinkingLevel::Low, "low"),
    (ThinkingLevel::Medium, "medium"),
    (ThinkingLevel::High, "high"),
    (ThinkingLevel::XHigh, "xhigh"),
];

fn thinking_levels(
    api: Api,
    model: &DevModel,
    model_id: &str,
    max_tokens: u64,
) -> Option<BTreeMap<ThinkingLevel, Option<String>>> {
    if !model.reasoning {
        return None;
    }
    // Our Anthropic provider drives thinking via `budget_tokens` for older
    // models, or `output_config.effort` for adaptive thinking models.
    if api == Api::AnthropicMessages {
        if is_anthropic_adaptive(model_id) {
            // Adaptive thinking: effort-based, not budget-based.
            let mut map: BTreeMap<_, _> = ANTHROPIC_ADAPTIVE_EFFORTS
                .iter()
                .map(|(level, effort)| (*level, Some((*effort).to_owned())))
                .collect();
            map.insert(
                ThinkingLevel::XHigh,
                Some(adaptive_xhigh_effort(model_id).to_owned()),
            );
            return Some(map);
        }
        // Budget-based thinking for older Anthropic models.
        let map: BTreeMap<_, _> = ANTHROPIC_BUDGETS
            .iter()
            .filter(|(_, budget)| *budget < max_tokens)
            .map(|(level, budget)| (*level, Some(budget.to_string())))
            .collect();
        return (!map.is_empty()).then_some(map);
    }
    let effort_values = model.reasoning_options.iter().find_map(|o| match o {
        ReasoningOption::Effort { values } => Some(values),
        _ => None,
    })?;
    let map: BTreeMap<_, _> = EFFORT_LEVELS
        .iter()
        .filter(|(_, value)| effort_values.iter().flatten().any(|v| v == value))
        .map(|(level, value)| (*level, Some((*value).to_owned())))
        .collect();
    (!map.is_empty()).then_some(map)
}

pub(super) fn convert(provider: &DevProvider, api: Api, base_url: &str) -> Vec<Model> {
    let mut models = Vec::new();
    for (id, m) in &provider.models {
        if !m.tool_call {
            continue;
        }
        let limit = m.limit.as_ref();
        let max_tokens = limit.and_then(|l| l.output).unwrap_or(4096);
        let context_window = limit.and_then(|l| l.context).unwrap_or(4096);
        // Apply per-(provider, model) overrides where upstream data disagrees
        // with the actual server. See `OVERRIDES` for known bad cases.
        let (context_window, max_tokens) =
            override_for(&provider.id, id).unwrap_or((context_window, max_tokens));
        let cost = m.cost.as_ref();
        let input = match &m.modalities {
            Some(mods) if mods.input.iter().any(|i| i == "image") => {
                vec![Modality::Text, Modality::Image]
            }
            _ => vec![Modality::Text],
        };
        models.push(Model {
            id: id.clone(),
            name: m.name.clone().unwrap_or_else(|| id.clone()),
            api,
            provider: provider.id.clone(),
            base_url: base_url.to_owned(),
            reasoning: m.reasoning,
            input,
            cost: CostRates {
                input: cost.map_or(0.0, |c| c.input),
                output: cost.map_or(0.0, |c| c.output),
                cache_read: cost.map_or(0.0, |c| c.cache_read),
                cache_write: cost.map_or(0.0, |c| c.cache_write),
            },
            context_window,
            max_tokens,
            headers: BTreeMap::new(),
            thinking_levels: thinking_levels(api, m, id, max_tokens),
            adaptive_thinking: api == Api::AnthropicMessages && is_anthropic_adaptive(id),
        });
    }
    models
}

pub(super) fn fetch_providers(
    runtime: &tokio::runtime::Runtime,
) -> Result<BTreeMap<String, DevProvider>> {
    runtime.block_on(async {
        eprintln!("fetching {MODELS_DEV_URL}...");
        fetch_capped_json(MODELS_DEV_URL).await
    })
}
