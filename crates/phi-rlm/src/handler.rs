//! The recursion primitive (`LanguageModel`) and the per-completion
//! `LmHandler` that routes sub-calls and rolls up usage across the tree.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use phi_ai::Usage;

use crate::{ChatMessage, Role};

#[derive(Clone, Debug, Default)]
pub struct Completion {
    pub text: String,
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
    async fn complete(&self, prompt: &[ChatMessage]) -> Result<Completion, String>;

    /// Stream text deltas through `on_delta` while completing. Models without
    /// streaming support fall back to a single non-streamed completion.
    async fn complete_streaming(
        &self,
        prompt: &[ChatMessage],
        on_delta: DeltaSink,
        on_thinking: Option<ThinkingSink>,
    ) -> Result<Completion, String> {
        let _ = (on_delta, on_thinking);
        self.complete(prompt).await
    }
}

/// Per-completion handler: root model, default sub model, optional named
/// models (`llm_query(prompt, model="...")`), cumulative usage.
pub struct LmHandler {
    root: Arc<dyn LanguageModel>,
    sub: Arc<dyn LanguageModel>,
    named: HashMap<String, Arc<dyn LanguageModel>>,
    usage: Mutex<Usage>,
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
            usage: Mutex::new(Usage::default()),
        }
    }

    pub fn root_name(&self) -> String {
        self.root.name()
    }

    pub fn usage(&self) -> Usage {
        *self.usage.lock().unwrap()
    }

    pub fn track(&self, usage: &Usage) {
        self.usage.lock().unwrap().add(usage);
    }

    pub async fn complete_root(
        &self,
        prompt: &[ChatMessage],
        on_delta: Option<DeltaSink>,
        on_thinking: Option<ThinkingSink>,
    ) -> Result<Completion, String> {
        let completion = match (on_delta, on_thinking) {
            (Some(sink), thinking) => self.root.complete_streaming(prompt, sink, thinking).await?,
            (None, Some(thinking)) => {
                self.root
                    .complete_streaming(prompt, Arc::new(|_: &str| true), Some(thinking))
                    .await?
            }
            (None, None) => self.root.complete(prompt).await?,
        };
        self.track(&completion.usage);
        Ok(completion)
    }

    /// Sub-LLM completion for `llm_query` (and `rlm_query` at max depth).
    /// Unknown model names fall back to the default sub model.
    pub async fn complete_sub(
        &self,
        prompt: &str,
        model: Option<&str>,
    ) -> Result<Completion, String> {
        let lm = model
            .and_then(|name| self.named.get(name))
            .unwrap_or(&self.sub);
        let messages = [ChatMessage {
            role: Role::User,
            content: prompt.to_owned(),
        }];
        let completion = lm.complete(&messages).await?;
        self.track(&completion.usage);
        Ok(completion)
    }

    pub fn sub_name(&self, model: Option<&str>) -> String {
        model
            .and_then(|name| self.named.get(name))
            .unwrap_or(&self.sub)
            .name()
    }
}
