//! Provider-backed `LanguageModel`: completions through the phi-ai registry,
//! with auth resolved per call (so OAuth tokens refresh mid-run).

use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use phi_ai::{
    AssistantContent, AssistantEvent, AssistantMessage, AuthStorage, Context, Message, Model,
    Registry, StreamOptions, ThinkingLevel, UserMessage,
};

use crate::handler::{Completion, DeltaSink, LanguageModel};
use crate::{ChatMessage, Role};

pub struct ProviderLm {
    registry: Arc<Registry>,
    model: RwLock<Model>,
    auth: Arc<AuthStorage>,
    thinking: RwLock<ThinkingLevel>,
}

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
    // The id (not the display name) is what `llm_query(model=...)` matches.
    fn name(&self) -> String {
        self.model.read().unwrap().id.clone()
    }

    async fn complete(&self, prompt: &[ChatMessage]) -> Result<Completion, String> {
        self.complete_streaming(prompt, std::sync::Arc::new(|_: &str| true))
            .await
    }

    async fn complete_streaming(
        &self,
        prompt: &[ChatMessage],
        on_delta: DeltaSink,
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
