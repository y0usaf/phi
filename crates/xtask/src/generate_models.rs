use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context as _, Result};
use phi_ai::types::{Api, CostRates, Modality, Model, ThinkingLevel};
use serde::Deserialize;

const MODELS_DEV_URL: &str = "https://models.dev/api.json";

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
            context_window: limit.and_then(|l| l.context).unwrap_or(4096),
            max_tokens,
            headers: BTreeMap::new(),
            thinking_levels: thinking_levels(api, m, max_tokens),
        });
    }
    models
}

pub fn run() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let providers: BTreeMap<String, DevProvider> = runtime.block_on(async {
        eprintln!("fetching {MODELS_DEV_URL}...");
        reqwest::get(MODELS_DEV_URL)
            .await
            .context("fetching models.dev")?
            .error_for_status()?
            .json()
            .await
            .context("parsing models.dev response")
    })?;

    let mut models = Vec::new();
    let mut skipped = Vec::new();
    for provider in providers.values() {
        match classify(provider) {
            Some((api, base_url)) => models.extend(convert(provider, api, &base_url)),
            None => skipped.push(provider.id.clone()),
        }
    }
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
        providers.len() - skipped.len(),
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
