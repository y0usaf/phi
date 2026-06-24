use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context as _, Result};
use phi_ai::types::{Api, CostRates, Modality, Model, ThinkingLevel};
use serde::Deserialize;

const MODELS_DEV_URL: &str = "https://models.dev/api.json";
const AI_GATEWAY_MODELS_URL: &str = "https://ai-gateway.vercel.sh/v1/models";
const AI_GATEWAY_BASE_URL: &str = "https://ai-gateway.vercel.sh";

/// Maximum bytes accepted from a model-listing endpoint. These are dev-only
/// manual fetches against hardcoded HTTPS URLs; the cap bounds memory if a
/// compromised endpoint or TLS MITM streams an oversized body.
const MAX_MODELS_JSON_BYTES: usize = 64 * 1024 * 1024;

/// Per-model fixes for upstream listings that disagree with the runtime
/// server. Sourced from `limit.context`/`limit.output` is generally right,
/// but some providers advertise limits larger than what they actually serve.
/// Entries are `(provider_id, model_id, context_window, max_tokens)`.
///
/// Known bad cases as of 2026-06: neuralwatt's `/v1/models` lists
/// `glm-5.2` with max_context_length = 1048560 but their vLLM serving
/// endpoint enforces `max_model_len = max_total_tokens = 200000`.
const OVERRIDES: &[(&str, &str, u64, u64)] = &[
    ("neuralwatt", "glm-5.2", 200_000, 65_536),
];

fn override_for(provider: &str, model: &str) -> Option<(u64, u64)> {
    OVERRIDES
        .iter()
        .find(|(p, m, _, _)| *p == provider && *m == model)
        .map(|(_, _, ctx, max)| (*ctx, *max))
}

/// Fetch and parse a JSON model listing with a body-size cap. `reqwest`'s
/// `.json()` buffers the entire response with no limit, so we read the bytes
/// explicitly and reject anything over `MAX_MODELS_JSON_BYTES` before parsing.
async fn fetch_capped_json<T: serde::de::DeserializeOwned>(url: &str) -> Result<T> {
    use futures::StreamExt as _;
    let response = reqwest::get(url)
        .await
        .context("fetching model listing")?
        .error_for_status()?;
    let mut buf: Vec<u8> = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("reading model-list body")?;
        buf.extend_from_slice(&chunk);
        if buf.len() > MAX_MODELS_JSON_BYTES {
            anyhow::bail!(
                "model-list response from {url} exceeded {} bytes",
                MAX_MODELS_JSON_BYTES
            );
        }
    }
    serde_json::from_slice(&buf).context("parsing model-list response")
}

#[derive(Deserialize)]
struct DevProvider {
    id: String,
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

fn classify(provider: &DevProvider) -> Option<(Api, String)> {
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
    max_tokens: u64,
) -> Option<BTreeMap<ThinkingLevel, Option<String>>> {
    if !model.reasoning {
        return None;
    }
    // Our Anthropic provider drives thinking via `budget_tokens`.
    if api == Api::AnthropicMessages {
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

fn convert(provider: &DevProvider, api: Api, base_url: &str) -> Vec<Model> {
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
        let (context_window, max_tokens) = override_for(&provider.id, id)
            .unwrap_or((context_window, max_tokens));
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
            thinking_levels: thinking_levels(api, m, max_tokens),
        });
    }
    models
}

fn fetch_ai_gateway_models(runtime: &tokio::runtime::Runtime) -> Result<Vec<Model>> {
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

pub fn run() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let providers: BTreeMap<String, DevProvider> = runtime.block_on(async {
        eprintln!("fetching {MODELS_DEV_URL}...");
        fetch_capped_json(MODELS_DEV_URL).await
    })?;

    let mut models = Vec::new();
    let mut skipped = Vec::new();
    for provider in providers.values() {
        match classify(provider) {
            Some((api, base_url)) => models.extend(convert(provider, api, &base_url)),
            None => skipped.push(provider.id.clone()),
        }
    }
    let ai_gateway_models = fetch_ai_gateway_models(&runtime)?;
    let extra_provider_count = usize::from(!ai_gateway_models.is_empty());
    models.extend(ai_gateway_models);
    models.sort_by(|a, b| (&a.provider, &a.id).cmp(&(&b.provider, &b.id)));

    let out_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../phi-ai/src/models.json");
    let mut json = serde_json::to_string_pretty(&models)?;
    json.push('\n');
    std::fs::write(&out_path, json)?;

    let mut per_api: BTreeMap<String, usize> = BTreeMap::new();
    for m in &models {
        *per_api
            .entry(serde_json::to_string(&m.api)?.trim_matches('"').to_owned())
            .or_default() += 1;
    }
    eprintln!(
        "wrote {} models from {} providers to {}",
        models.len(),
        providers.len() - skipped.len() + extra_provider_count,
        out_path.display()
    );
    for (api, count) in &per_api {
        eprintln!("  {api}: {count}");
    }
    if !skipped.is_empty() {
        eprintln!("skipped providers (no static base URL or unsupported protocol):");
        eprintln!("  {}", skipped.join(", "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_for_known_bad_neuralwatt_glm_5_2() {
        // models.dev advertises 1048560 for both; vLLM actually serves 200k.
        let (ctx, max) = override_for("neuralwatt", "glm-5.2").expect("override present");
        assert_eq!(ctx, 200_000);
        assert_eq!(max, 65_536);
    }

    #[test]
    fn override_for_unknown_returns_none() {
        assert!(override_for("anthropic", "claude-opus-4-7").is_none());
    }
}
