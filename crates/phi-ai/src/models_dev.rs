//! models.dev catalog ingestion: fetch, classify, and convert to `Model`.
//!
//! Shared by two callers with the same guarantees:
//! - `xtask generate-models`: regenerates the embedded `models.json` +
//!   the bundled third-party `catalog.json` at build time.
//! - `phi --update-models`: refreshes a local runtime cache of the
//!   first-party models (`Registry::refresh`) without a rebuild.

use anyhow::{Context as _, Result};
use std::time::Duration;

/// Maximum bytes accepted from a model-listing endpoint. These are dev-only
/// manual fetches against hardcoded HTTPS URLs; the cap bounds memory if a
/// compromised endpoint or TLS MITM streams an oversized body.
pub const MAX_MODELS_JSON_BYTES: usize = 64 * 1024 * 1024;

/// Whole-request deadline: connect + headers + body must finish inside this
/// budget, or the fetch is abandoned (a stalled endpoint must not hang the
/// catalog regen).
const FETCH_TIMEOUT: Duration = Duration::from_secs(120);

/// Fetch and parse a JSON model listing with a body-size cap (`reqwest`'s
/// `.json()` buffers the whole response with no limit) and a whole-request
/// deadline (a stalled endpoint must not hang the regen).
pub async fn fetch_capped_json<T: serde::de::DeserializeOwned>(url: &str) -> Result<T> {
    use futures::StreamExt as _;
    tokio::time::timeout(FETCH_TIMEOUT, async {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .context("building HTTP client")?;
        let response = client
            .get(url)
            .timeout(FETCH_TIMEOUT)
            .send()
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
    })
    .await
    .context("model-list fetch timed out")?
}

use std::collections::BTreeMap;

use crate::types::{Api, CostRates, Modality, Model, ThinkingLevel};
use serde::Deserialize;

/// First-party providers whose models Phi embeds in `phi-ai` — the ones Phi
/// has native auth flows for. Every other provider that classifies (i.e.
/// exposes a plain API-key endpoint one of Phi's backends can speak) goes
/// into the bundled catalog, which `phi-builtins` registers through the same
/// `register_provider` API user extensions use.
pub const FIRST_PARTY_PROVIDERS: &[&str] = &["anthropic", "openai"];

const MODELS_DEV_URL: &str = "https://models.dev/api.json";

/// Per-model fixes for upstream listings that disagree with the runtime
/// server. Sourced from `limit.context`/`limit.output` is generally right,
/// but providers sometimes advertise limits larger than what they actually
/// serve. Entries are `(provider_id, model_id, context_window, max_tokens)`.
const OVERRIDES: &[(&str, &str, u64, u64)] = &[("neuralwatt", "glm-5.2", 200_000, 65_536)];

pub fn override_for(provider: &str, model: &str) -> Option<(u64, u64)> {
    OVERRIDES
        .iter()
        .find(|(p, m, _, _)| *p == provider && *m == model)
        .map(|(_, _, ctx, max)| (*ctx, *max))
}

#[derive(Deserialize)]
pub struct DevProvider {
    pub id: String,
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

/// Base URLs for providers whose models.dev entry has no `api` field (the
/// npm SDK encodes the URL instead). Beyond the two first-party entries,
/// these are documented OpenAI-compatible endpoints that plain API-key auth
/// drives — which is what qualifies a provider for the bundled catalog.
/// Providers whose endpoint depends on account/region/project config
/// (azure, vertex, bedrock) stay out: a static entry can't represent them.
fn builtin_base_url(provider_id: &str) -> Option<&'static str> {
    Some(match provider_id {
        "anthropic" => "https://api.anthropic.com",
        "openai" => "https://api.openai.com/v1",
        // Vercel AI Gateway: models.dev encodes the URL in `@ai-sdk/gateway`;
        // the gateway speaks plain OpenAI chat completions at /v1 with
        // AI_GATEWAY_API_KEY (see env_auth's special case).
        "vercel" => "https://ai-gateway.vercel.sh/v1",
        "google" => "https://generativelanguage.googleapis.com/v1beta/openai",
        "mistral" => "https://api.mistral.ai/v1",
        "groq" => "https://api.groq.com/openai/v1",
        "cerebras" => "https://api.cerebras.ai/v1",
        "xai" => "https://api.x.ai/v1",
        "togetherai" => "https://api.together.xyz/v1",
        "deepinfra" => "https://api.deepinfra.com/v1/openai",
        "perplexity" => "https://api.perplexity.ai",
        _ => return None,
    })
}

pub fn classify(provider: &DevProvider) -> Option<(Api, String)> {
    let api = match provider.npm.as_deref().unwrap_or("") {
        "@ai-sdk/anthropic" => Api::AnthropicMessages,
        "@ai-sdk/openai" => Api::OpenAiResponses,
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
pub fn is_anthropic_adaptive(model_id: &str) -> bool {
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
pub fn adaptive_xhigh_effort(model_id: &str) -> &'static str {
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

pub fn convert(provider: &DevProvider, api: Api, base_url: &str) -> Vec<Model> {
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

pub async fn fetch_providers() -> Result<BTreeMap<String, DevProvider>> {
    eprintln!("fetching {MODELS_DEV_URL}...");
    fetch_capped_json(MODELS_DEV_URL).await
}
