use std::collections::BTreeMap;
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

/// Serde helper: skip serializing `false` bools (keeps JSON compact for the
/// default-valued `adaptive_thinking` field on `Model`).
fn is_false(b: &bool) -> bool {
    !b
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum Api {
    #[serde(rename = "anthropic-messages")]
    AnthropicMessages,
    #[serde(rename = "openai-responses")]
    OpenAiResponses,
    #[serde(rename = "openai-completions")]
    OpenAiCompletions,
    #[serde(rename = "bedrock-converse-stream")]
    BedrockConverseStream,
    #[serde(rename = "google-generative-ai")]
    GoogleGenerativeAi,
    #[serde(rename = "google-vertex")]
    GoogleVertex,
    #[serde(rename = "mistral-conversations")]
    MistralConversations,
    #[serde(rename = "cursor-native")]
    CursorNative,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    #[default]
    Medium,
    Off,
    Minimal,
    Low,
    High,
    XHigh,
}

impl ThinkingLevel {
    pub const ALL: [Self; 6] = [
        Self::Off,
        Self::Minimal,
        Self::Low,
        Self::Medium,
        Self::High,
        Self::XHigh,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
        }
    }
}

impl FromStr for ThinkingLevel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "off" => Ok(Self::Off),
            "minimal" => Ok(Self::Minimal),
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "xhigh" => Ok(Self::XHigh),
            other => Err(format!("unknown thinking level '{other}'")),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Modality {
    Text,
    Image,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CostRates {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Model {
    pub id: String,
    pub name: String,
    pub api: Api,
    pub provider: String,
    pub base_url: String,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub input: Vec<Modality>,
    /// Dollars per million tokens.
    #[serde(default)]
    pub cost: CostRates,
    pub context_window: u64,
    pub max_tokens: u64,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Per-model mapping of thinking level -> provider value (e.g. effort
    /// string or token budget). `None` value = level unsupported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_levels: Option<BTreeMap<ThinkingLevel, Option<String>>>,
    /// Whether this model uses Anthropic adaptive thinking (`thinking.type:
    /// "adaptive"` + `output_config.effort`) instead of budget-based thinking
    /// (`thinking.type: "enabled"` + `budget_tokens`). When true,
    /// `thinking_levels` stores effort strings (e.g. "low", "medium",
    /// "high", "xhigh", "max") rather than token budgets.
    #[serde(default, skip_serializing_if = "is_false")]
    pub adaptive_thinking: bool,
    /// Declares how to write reasoning control into an OpenAI-Completions
    /// request body. The wire field name varies by upstream dialect
    /// (`chat_template_kwargs.enable_thinking` for vLLM/Qwen/GLM,
    /// `reasoning_effort` for OpenAI-shaped routers), so the field path
    /// and the value for each `ThinkingLevel` are declared per model
    /// rather than hand-wired per provider (doctrine 05). The Completions
    /// provider writes the resolved value at the dotted `field` path;
    /// Anthropic/Responses providers ignore this and keep using
    /// `thinking_value`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_control: Option<ReasoningControl>,
}

/// Per-model declaration of the OpenAI-Completions reasoning-control wire
/// field. `field` is a dotted path into the request body (e.g.
/// `"chat_template_kwargs.enable_thinking"`); `values` maps each
/// `ThinkingLevel` to the literal JSON value written at that path.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReasoningControl {
    /// Dotted path into the Chat Completions request body.
    pub field: String,
    /// Thinking level → literal JSON value at `field`. A level absent from
    /// this map is not written (the model keeps its upstream default).
    #[serde(default)]
    pub values: BTreeMap<ThinkingLevel, serde_json::Value>,
}

impl Model {
    /// Clamp to the nearest supported level at or below the requested one,
    /// returning the provider-specific value for it.
    pub fn thinking_value(&self, level: ThinkingLevel) -> Option<String> {
        if level == ThinkingLevel::Off || !self.reasoning {
            return None;
        }
        // If the model has explicit thinking levels, clamp to the nearest
        // supported level at or below the requested one.
        if let Some(map) = self.thinking_levels.as_ref() {
            let mut candidates: Vec<&ThinkingLevel> = map
                .iter()
                .filter(|(l, v)| **l != ThinkingLevel::Off && v.is_some() && **l <= level)
                .map(|(l, _)| l)
                .collect();
            candidates.sort();
            let chosen = candidates.last()?;
            return map.get(chosen).cloned().flatten();
        }
        // Fallback: the model supports reasoning but has no explicit thinking
        // levels (e.g. Vercel AI Gateway models where the gateway API doesn't
        // expose reasoning_options metadata). Without this, thinking would be
        // silently dropped for all such models regardless of the configured
        // thinking level. Use sensible defaults so thinking is actually sent.
        Some(default_thinking_value(self, level))
    }

    /// Levels a user may cycle through for this model (pi's
    /// `getSupportedThinkingLevels`). A non-reasoning model gets `[Off]`;
    /// a reasoning model gets `Off` plus every level it can take, in order.
    /// `Off` is always present for a reasoning model so the cycle can reach
    /// the off state from the TUI.
    pub fn supported_thinking_levels(&self) -> Vec<ThinkingLevel> {
        if !self.reasoning {
            return vec![ThinkingLevel::Off];
        }
        let mut out = vec![ThinkingLevel::Off];
        match self.thinking_levels.as_ref() {
            Some(map) => {
                for level in ThinkingLevel::ALL {
                    if level == ThinkingLevel::Off {
                        continue;
                    }
                    // A level is supported when the map carries a value
                    // for it (None value = explicitly unsupported, matching
                    // `thinking_value`).
                    if map.get(&level).is_some_and(|v| v.is_some()) {
                        out.push(level);
                    }
                }
            }
            None => {
                // No explicit map: the fallback path resolves any non-Off
                // level, so all are cyclable.
                for level in ThinkingLevel::ALL {
                    if level != ThinkingLevel::Off {
                        out.push(level);
                    }
                }
            }
        }
        out
    }
}

/// Default thinking value when a model has `reasoning: true` but no explicit
/// `thinking_levels` map. Returns a provider-appropriate value:
///
/// - **Adaptive thinking** / **OpenAI Responses**: the level name as an effort
///   string (e.g. `"high"`), matching how Pi's runtime falls back to the effort
///   level when no `thinkingLevelMap` is baked.
/// - **Budget-based thinking** (non-adaptive Anthropic Messages): a default
///   token budget string (e.g. `"16384"`), matching `ANTHROPIC_BUDGETS` in
///   `models_dev.rs` and Pi's `ANTHROPIC_THINKING` defaults.
fn default_thinking_value(model: &Model, level: ThinkingLevel) -> String {
    if model.adaptive_thinking || model.api == Api::OpenAiResponses {
        level.as_str().to_owned()
    } else {
        default_thinking_budget(level).to_string()
    }
}

/// Default token budgets for budget-based thinking (non-adaptive Anthropic
/// Messages models). Matches the `ANTHROPIC_BUDGETS` table in
/// `xtask/generate_models/models_dev.rs` and Pi's `ANTHROPIC_THINKING`.
fn default_thinking_budget(level: ThinkingLevel) -> u64 {
    match level {
        ThinkingLevel::Off => 0,
        ThinkingLevel::Minimal => 1024,
        ThinkingLevel::Low => 4096,
        ThinkingLevel::Medium => 8192,
        ThinkingLevel::High => 16384,
        ThinkingLevel::XHigh => 32768,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Cost {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub total: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub total_tokens: u64,
    pub cost: Cost,
}

impl Usage {
    pub fn finalize(&mut self, rates: &CostRates) {
        self.total_tokens = self.input + self.output + self.cache_read + self.cache_write;
        self.cost.input = self.input as f64 * rates.input / 1e6;
        self.cost.output = self.output as f64 * rates.output / 1e6;
        self.cost.cache_read = self.cache_read as f64 * rates.cache_read / 1e6;
        self.cost.cache_write = self.cache_write as f64 * rates.cache_write / 1e6;
        self.cost.total =
            self.cost.input + self.cost.output + self.cost.cache_read + self.cost.cache_write;
    }

    pub fn add(&mut self, other: &Usage) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
        self.total_tokens += other.total_tokens;
        self.cost.input += other.cost.input;
        self.cost.output += other.cost.output;
        self.cost.cache_read += other.cost.cache_read;
        self.cost.cache_write += other.cost.cache_write;
        self.cost.total += other.cost.total;
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    #[default]
    Stop,
    Length,
    ToolUse,
    Error,
    Aborted,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// A tool definition sent to the API so the model can call it natively.
/// The `input_schema` is a JSON Schema object (e.g. `{"type": "object", "properties": {...}}`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantContent {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    ToolCall(ToolCall),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserMessage {
    pub content: String,
    pub timestamp: i64,
}

impl UserMessage {
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            timestamp: now_ms(),
        }
    }
}

/// A tool result returned to the model, paired by `tool_use_id` with the
/// assistant's `ToolCall`. Carried as a user-role message in the API request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolResultMessage {
    /// Matches the `id` of the assistant `ToolCall` that produced this result.
    pub tool_use_id: String,
    /// The tool result content (stdout, return value, or error text).
    pub content: String,
    /// Whether the tool execution errored (providers may relay this to the model).
    #[serde(default)]
    pub is_error: bool,
    pub timestamp: i64,
}

impl ToolResultMessage {
    pub fn new(tool_use_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tool_use_id: tool_use_id.into(),
            content: content.into(),
            is_error: false,
            timestamp: now_ms(),
        }
    }

    pub fn with_error(mut self, is_error: bool) -> Self {
        self.is_error = is_error;
        self
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssistantMessage {
    pub content: Vec<AssistantContent>,
    pub api: Api,
    pub provider: String,
    pub model: String,
    pub usage: Usage,
    pub stop_reason: StopReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    pub timestamp: i64,
}

impl AssistantMessage {
    pub fn empty(model: &Model) -> Self {
        Self {
            content: Vec::new(),
            api: model.api,
            provider: model.provider.clone(),
            model: model.id.clone(),
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            error_message: None,
            timestamp: now_ms(),
        }
    }

    pub fn error(model: &Model, stop_reason: StopReason, error: impl Into<String>) -> Self {
        let mut message = Self::empty(model);
        message.stop_reason = stop_reason;
        message.error_message = Some(error.into());
        message
    }

    pub fn text(&self) -> String {
        let mut out = String::new();
        for block in &self.content {
            if let AssistantContent::Text { text } = block {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            }
        }
        out
    }

    pub fn is_error(&self) -> bool {
        matches!(self.stop_reason, StopReason::Error | StopReason::Aborted)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    User(UserMessage),
    Assistant(AssistantMessage),
    /// A tool result returned to the model, paired by `tool_use_id`.
    /// Serialized as a user-role message with `tool_result` content blocks
    /// by each provider's `convert_messages`.
    ToolResult(ToolResultMessage),
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Context {
    pub system_prompt: Option<String>,
    pub messages: Vec<Message>,
    /// Tool definitions sent to the API so the model can call them natively
    /// via `tool_use` blocks instead of text-based fences.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolDefinition>,
}

#[derive(Clone, Debug)]
pub enum Auth {
    ApiKey(String),
    /// OAuth access token (Anthropic Pro/Max). Providers adjust
    /// headers/payload accordingly.
    OAuthToken(String),
    /// ChatGPT Codex subscription token; requests go to the ChatGPT backend
    /// with the account id header.
    ChatGptOAuth {
        access: String,
        account_id: String,
    },
}

impl Auth {
    pub fn secret(&self) -> &str {
        match self {
            Self::ApiKey(s) | Self::OAuthToken(s) => s,
            Self::ChatGptOAuth { access, .. } => access,
        }
    }
}

#[derive(Clone, Debug)]
pub struct StreamOptions {
    pub auth: Auth,
    pub max_tokens: Option<u64>,
    pub temperature: Option<f64>,
    pub thinking: ThinkingLevel,
    pub cancel: CancellationToken,
    pub timeout: Duration,
    pub max_retries: u32,
    /// Identity string injected as the uncached first system block for
    /// OAuth-authenticated Anthropic requests. `None` (default) falls back
    /// to the provider's Claude Code compliance shim, which the API requires
    /// before it will honor an OAuth token.
    pub identity: Option<String>,
    /// Whether the provider should apply `cache_control` breakpoints
    /// (Anthropic `ephemeral` markers) to the system prompt and last
    /// user message. Default `true`.
    pub cache_control: bool,
}

impl StreamOptions {
    pub fn new(auth: Auth) -> Self {
        Self {
            auth,
            max_tokens: None,
            temperature: None,
            thinking: ThinkingLevel::Medium,
            cancel: CancellationToken::new(),
            timeout: Duration::from_secs(300),
            max_retries: 3,
            identity: None,
            cache_control: true,
        }
    }
}

pub fn now_ms() -> i64 {
    jiff::Timestamp::now().as_millisecond()
}
