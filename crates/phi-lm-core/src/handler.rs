//! The recursion primitive (`LanguageModel`) and the per-completion
//! `LmHandler` that routes sub-calls and rolls up usage across the tree.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use phi_ai::Usage;
use tokio_util::sync::CancellationToken;

use crate::{ChatMessage, Role};

#[derive(Clone, Debug, Default)]
pub struct Completion {
    pub text: String,
    pub usage: Usage,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UsageBreakdown {
    pub model: String,
    pub usage: Usage,
}

/// Receives streamed text chunks during a completion. Returns `false` to
/// stop generation early (the model drops the stream and returns the text
/// accumulated so far).
pub type DeltaSink = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// Receives streamed provider-thinking chunks during a completion.
/// Observational only: thinking never affects generation control flow.
pub type ThinkingSink = Arc<dyn Fn(&str) + Send + Sync>;

/// Implemented by plain provider-backed models (phi-runtime wires phi-ai in)
/// and by `Rlm` itself — recursion is just trait recursion.
#[async_trait]
pub trait LanguageModel: Send + Sync {
    fn name(&self) -> String;
    async fn complete(&self, prompt: &[ChatMessage], cancel: &CancellationToken) -> Result<Completion, String>;

    /// Stream text deltas through `on_delta` while completing. Models without
    /// streaming support fall back to a single non-streamed completion.
    async fn complete_streaming(
        &self,
        prompt: &[ChatMessage],
        on_delta: DeltaSink,
        on_thinking: Option<ThinkingSink>,
        cancel: &CancellationToken,
    ) -> Result<Completion, String> {
        let _ = (on_delta, on_thinking);
        self.complete(prompt, cancel).await
    }
}

#[derive(Clone, Default)]
struct UsageLedger {
    total: Usage,
    by_model: BTreeMap<String, Usage>,
}

/// Per-completion handler: root model, default sub model, optional named
/// models (`llm_query(prompt, model="...")`), cumulative usage.
pub struct LmHandler {
    root: Arc<dyn LanguageModel>,
    sub: Arc<dyn LanguageModel>,
    named: HashMap<String, Arc<dyn LanguageModel>>,
    usage: Mutex<UsageLedger>,
}

impl LmHandler {
    pub fn new(
        root: Arc<dyn LanguageModel>,
        sub: Arc<dyn LanguageModel>,
        named: HashMap<String, Arc<dyn LanguageModel>>,
    ) -> Self {
        Self {
            root,
            sub,
            named,
            usage: Mutex::new(UsageLedger::default()),
        }
    }

    pub fn root_name(&self) -> String {
        self.root.name()
    }

    pub fn usage(&self) -> Usage {
        self.usage.lock().unwrap_or_else(|e| e.into_inner()).total
    }

    /// Add usage from a child RLM completion (rolls up the recursion tree).
    pub fn add_child_usage(&self, child_usage: &Usage) {
        self.usage.lock().unwrap_or_else(|e| e.into_inner()).total.add(child_usage);
    }

    pub fn usage_by_model(&self) -> Vec<UsageBreakdown> {
        Self::breakdowns_from_ledger(&self.usage.lock().unwrap_or_else(|e| e.into_inner()).by_model)
    }

    pub fn usage_since(&self, snapshot: &Usage) -> Usage {
        let usage = self.usage();
        usage_delta(&usage, snapshot)
    }

    pub fn usage_by_model_since(&self, snapshot: &[UsageBreakdown]) -> Vec<UsageBreakdown> {
        let current = self.usage.lock().unwrap_or_else(|e| e.into_inner()).by_model.clone();
        let mut previous = BTreeMap::<String, Usage>::new();
        for item in snapshot {
            previous.insert(item.model.clone(), item.usage);
        }
        Self::breakdowns_from_ledger(
            &current
                .into_iter()
                .filter_map(|(model, usage)| {
                    let delta =
                        usage_delta(&usage, previous.get(&model).unwrap_or(&Usage::default()));
                    usage_has_tokens_or_cost(&delta).then_some((model, delta))
                })
                .collect(),
        )
    }

    fn breakdowns_from_ledger(ledger: &BTreeMap<String, Usage>) -> Vec<UsageBreakdown> {
        ledger
            .iter()
            .filter(|(_, usage)| usage_has_tokens_or_cost(usage))
            .map(|(model, usage)| UsageBreakdown {
                model: model.clone(),
                usage: *usage,
            })
            .collect()
    }

    pub fn track_model(&self, model: impl Into<String>, usage: &Usage) {
        if !usage_has_tokens_or_cost(usage) {
            return;
        }
        let mut ledger = self.usage.lock().unwrap_or_else(|e| e.into_inner());
        ledger.total.add(usage);
        ledger.by_model.entry(model.into()).or_default().add(usage);
    }

    pub fn track_breakdowns(&self, breakdowns: &[UsageBreakdown]) {
        for breakdown in breakdowns {
            self.track_model(breakdown.model.clone(), &breakdown.usage);
        }
    }

    pub async fn complete_root(
        &self,
        prompt: &[ChatMessage],
        on_delta: Option<DeltaSink>,
        on_thinking: Option<ThinkingSink>,
        cancel: &CancellationToken,
    ) -> Result<Completion, String> {
        let model = self.root.name();
        let completion = match (on_delta, on_thinking) {
            (Some(sink), thinking) => self.root.complete_streaming(prompt, sink, thinking, cancel).await?,
            (None, Some(thinking)) => {
                self.root
                    .complete_streaming(prompt, Arc::new(|_: &str| true), Some(thinking), cancel)
                    .await?
            }
            (None, None) => self.root.complete(prompt, cancel).await?,
        };
        self.track_model(model, &completion.usage);
        Ok(completion)
    }

    /// Sub-LLM completion for `llm_query` (and `rlm_query` at max depth).
    /// Unknown model names fall back to the default sub model.
    pub async fn complete_sub(
        &self,
        prompt: &str,
        model: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<Completion, String> {
        let lm = model
            .and_then(|name| self.named.get(name))
            .unwrap_or(&self.sub);
        let model_name = lm.name();
        let messages = [ChatMessage {
            role: Role::User,
            content: prompt.to_owned(),
        }];
        let completion = lm.complete(&messages, cancel).await?;
        self.track_model(model_name, &completion.usage);
        Ok(completion)
    }

    pub fn sub_name(&self, model: Option<&str>) -> String {
        model
            .and_then(|name| self.named.get(name))
            .unwrap_or(&self.sub)
            .name()
    }
}

fn usage_delta(current: &Usage, previous: &Usage) -> Usage {
    Usage {
        input: current.input.saturating_sub(previous.input),
        output: current.output.saturating_sub(previous.output),
        cache_read: current.cache_read.saturating_sub(previous.cache_read),
        cache_write: current.cache_write.saturating_sub(previous.cache_write),
        total_tokens: current.total_tokens.saturating_sub(previous.total_tokens),
        cost: phi_ai::Cost {
            input: current.cost.input - previous.cost.input,
            output: current.cost.output - previous.cost.output,
            cache_read: current.cost.cache_read - previous.cost.cache_read,
            cache_write: current.cost.cache_write - previous.cost.cache_write,
            total: current.cost.total - previous.cost.total,
        },
    }
}

fn usage_has_tokens_or_cost(usage: &Usage) -> bool {
    usage.total_tokens > 0
        || usage.input > 0
        || usage.output > 0
        || usage.cache_read > 0
        || usage.cache_write > 0
        || usage.cost.total != 0.0
}
