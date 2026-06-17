//! Provider-backed `LanguageModel`: completions through the phi-ai registry,
//! with auth resolved per call (so OAuth tokens refresh mid-run).

use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use phi_ai::{
    AssistantContent, AssistantEvent, AssistantMessage, AuthStorage, Context, Message, Model,
    Registry, StreamOptions, ThinkingLevel, UserMessage,
};

use crate::handler::{Completion, DeltaSink, LanguageModel, ThinkingSink};
use crate::{ChatMessage, Role};

pub struct ProviderLm {
    registry: Arc<Registry>,
    model: RwLock<Model>,
    auth: Arc<AuthStorage>,
    thinking: RwLock<ThinkingLevel>,
}

/// Transient-looking provider failures are retried; auth/4xx errors are not.
fn is_transient(error: &str) -> bool {
    let e = error.to_ascii_lowercase();
    let codes = ["429", "500", "502", "503", "504", "529"];
    codes.iter().any(|c| e.contains(c))
        || e.contains("overloaded")
        || e.contains("rate limit")
        || e.contains("rate_limit")
        || e.contains("timed out")
        || e.contains("timeout")
        || e.contains("connection")
        || e.contains("temporarily")
        || e.contains("stream ended without a terminal event")
}

const RETRIES: u32 = 3;
const BASE_BACKOFF_MS: u64 = 500;

impl ProviderLm {
    pub fn new(
        registry: Arc<Registry>,
        model: Model,
        auth: Arc<AuthStorage>,
        thinking: ThinkingLevel,
    ) -> Self {
        Self {
            registry,
            model: RwLock::new(model),
            auth,
            thinking: RwLock::new(thinking),
        }
    }

    pub fn model(&self) -> Model {
        self.model.read().unwrap().clone()
    }

    pub fn thinking(&self) -> ThinkingLevel {
        *self.thinking.read().unwrap()
    }

    /// Swap the thinking level; takes effect on the next completion.
    pub fn set_thinking(&self, level: ThinkingLevel) {
        *self.thinking.write().unwrap() = level;
    }

    /// Swap the model; takes effect on the next completion (auth is
    /// resolved per call, so no other state needs rebuilding).
    pub fn set_model(&self, model: Model) {
        *self.model.write().unwrap() = model;
    }

    fn context(model: &Model, prompt: &[ChatMessage]) -> Context {
        let mut ctx = Context::default();
        for message in prompt {
            match message.role {
                Role::System => match &mut ctx.system_prompt {
                    Some(existing) => {
                        existing.push_str("\n\n");
                        existing.push_str(&message.content);
                    }
                    None => ctx.system_prompt = Some(message.content.clone()),
                },
                Role::User => ctx
                    .messages
                    .push(Message::User(UserMessage::new(message.content.clone()))),
                Role::Assistant => {
                    let mut assistant = AssistantMessage::empty(model);
                    assistant.content.push(AssistantContent::Text {
                        text: message.content.clone(),
                    });
                    ctx.messages.push(Message::Assistant(assistant));
                }
            }
        }
        ctx
    }
}

#[async_trait]
impl LanguageModel for ProviderLm {
    // Provider-qualified so usage buckets don't collide across providers.
    fn name(&self) -> String {
        let model = self.model.read().unwrap();
        format!("{}/{}", model.provider, model.id)
    }

    async fn complete(&self, prompt: &[ChatMessage]) -> Result<Completion, String> {
        self.complete_streaming(prompt, std::sync::Arc::new(|_: &str| true), None)
            .await
    }

    async fn complete_streaming(
        &self,
        prompt: &[ChatMessage],
        on_delta: DeltaSink,
        on_thinking: Option<ThinkingSink>,
    ) -> Result<Completion, String> {
        // Retry-with-backoff on transient faults (429/5xx/transport), but
        // only while nothing has streamed: once deltas reached the caller,
        // retrying would duplicate visible output, so the error surfaces.
        let mut attempt = 0u32;
        loop {
            let streamed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let flag = streamed.clone();
            let inner = on_delta.clone();
            let wrapped: DeltaSink = std::sync::Arc::new(move |delta: &str| {
                flag.store(true, std::sync::atomic::Ordering::Relaxed);
                inner(delta)
            });
            match self
                .attempt_streaming(prompt, wrapped, on_thinking.clone())
                .await
            {
                Ok(completion) => return Ok(completion),
                Err(e)
                    if attempt < RETRIES
                        && is_transient(&e)
                        && !streamed.load(std::sync::atomic::Ordering::Relaxed) =>
                {
                    let backoff = BASE_BACKOFF_MS << attempt;
                    attempt += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(backoff)).await;
                }
                Err(e) => return Err(e),
            }
        }
    }
}

impl ProviderLm {
    async fn attempt_streaming(
        &self,
        prompt: &[ChatMessage],
        on_delta: DeltaSink,
        on_thinking: Option<ThinkingSink>,
    ) -> Result<Completion, String> {
        let model = self.model();
        let auth = self
            .auth
            .resolve(&model.provider)
            .await
            .map_err(|e| format!("auth resolution failed for '{}': {e}", model.provider))?
            .ok_or_else(|| {
                format!(
                    "no credentials for provider '{}' (use --login, --api-key, or env)",
                    model.provider
                )
            })?;

        let mut opts = StreamOptions::new(auth);
        opts.thinking = self.thinking();

        let context = Self::context(&model, prompt);
        let mut stream = self.registry.stream(&model, &context, opts);
        let mut terminal = None;
        let mut accumulated = String::new();
        while let Some(event) = stream.next().await {
            match event {
                AssistantEvent::TextDelta { delta, .. } => {
                    accumulated.push_str(&delta);
                    if !on_delta(&delta) {
                        // Early stop: dropping the stream aborts the request.
                        // Usage is unknown for aborted completions.
                        return Ok(Completion {
                            text: accumulated,
                            usage: phi_ai::Usage::default(),
                        });
                    }
                }
                AssistantEvent::ThinkingDelta { delta, .. } => {
                    if let Some(sink) = &on_thinking {
                        sink(&delta);
                    }
                }
                AssistantEvent::Done { message } | AssistantEvent::Error { message } => {
                    terminal = Some(message);
                }
                _ => {}
            }
        }
        let message = terminal.ok_or_else(|| "stream ended without a terminal event".to_owned())?;
        if message.is_error() {
            return Err(message
                .error_message
                .unwrap_or_else(|| "provider error".to_owned()));
        }
        Ok(Completion {
            text: message.text(),
            usage: message.usage,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::is_transient;

    #[test]
    fn transient_classification() {
        assert!(is_transient("HTTP 429 rate limit exceeded"));
        assert!(is_transient("server error: 503 Service Unavailable"));
        assert!(is_transient("Overloaded"));
        assert!(is_transient("connection reset by peer"));
        assert!(is_transient("request timed out"));
        assert!(is_transient("stream ended without a terminal event"));
        assert!(!is_transient("HTTP 401 unauthorized"));
        assert!(!is_transient("no credentials for provider 'anthropic'"));
        assert!(!is_transient("HTTP 400 invalid request"));
    }
}
