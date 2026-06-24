use std::collections::HashMap;
use std::sync::Arc;

use crate::stream::AssistantEventStream;
use crate::types::{Api, AssistantMessage, Context, Model, StreamOptions};

pub trait ApiProvider: Send + Sync {
    fn api(&self) -> Api;
    fn stream(&self, model: &Model, ctx: &Context, opts: StreamOptions) -> AssistantEventStream;
}

pub struct Registry {
    models: Vec<Model>,
    providers: HashMap<Api, Arc<dyn ApiProvider>>,
}

impl Registry {
    pub fn builtin() -> Self {
        let models: Vec<Model> = serde_json::from_str(include_str!("models.json"))
            .expect("embedded models.json must parse");
        let mut providers: HashMap<Api, Arc<dyn ApiProvider>> = HashMap::new();
        providers.insert(
            Api::AnthropicMessages,
            Arc::new(crate::providers::anthropic::AnthropicProvider::new()),
        );
        providers.insert(
            Api::OpenAiResponses,
            Arc::new(crate::providers::openai_responses::OpenAiResponsesProvider::new()),
        );
        providers.insert(
            Api::OpenAiCompletions,
            Arc::new(crate::providers::openai_completions::OpenAiCompletionsProvider::new()),
        );
        Self { models, providers }
    }

    pub fn models(&self) -> &[Model] {
        &self.models
    }

    /// True if a provider implementation is registered for the model's API.
    pub fn is_supported(&self, model: &Model) -> bool {
        self.providers.contains_key(&model.api)
    }

    /// Models that can actually be driven by a registered provider. Use this
    /// for user-facing listings; `models()` includes entries for APIs we
    /// don't implement yet.
    pub fn supported_models(&self) -> impl Iterator<Item = &Model> {
        self.models.iter().filter(|m| self.is_supported(m))
    }

    /// Look up by model id, optionally qualified as `provider/model-id`.
    /// Prefers supported models when ids collide across providers.
    pub fn find(&self, query: &str) -> Option<&Model> {
        if let Some((provider, id)) = query.split_once('/') {
            return self
                .models
                .iter()
                .find(|m| m.provider == provider && m.id == id);
        }
        self.supported_models()
            .find(|m| m.id == query)
            .or_else(|| self.models.iter().find(|m| m.id == query))
    }

    pub fn first_for_provider(&self, provider: &str) -> Option<&Model> {
        self.models.iter().find(|m| m.provider == provider)
    }

    pub fn stream(
        &self,
        model: &Model,
        ctx: &Context,
        opts: StreamOptions,
    ) -> AssistantEventStream {
        match self.providers.get(&model.api) {
            Some(provider) => provider.stream(model, ctx, opts),
            None => {
                let (sink, stream) = AssistantEventStream::channel();
                sink.error(
                    model,
                    crate::types::StopReason::Error,
                    format!("no provider registered for api {:?}", model.api),
                );
                stream
            }
        }
    }

    pub async fn complete(
        &self,
        model: &Model,
        ctx: &Context,
        opts: StreamOptions,
    ) -> AssistantMessage {
        self.stream(model, ctx, opts).result().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_models_parse_and_resolve() {
        let registry = Registry::builtin();
        assert!(registry.models().len() >= 4);
        assert!(registry.find("claude-opus-4-7").is_some());
        assert!(registry.find("anthropic/claude-opus-4-7").is_some());
        assert!(registry.find("openai/gpt-5.1").is_some());
        assert!(registry.find("nope").is_none());
    }

    #[test]
    fn supported_models_filter_by_registered_provider() {
        let registry = Registry::builtin();
        let total = registry.models().len();
        let supported: Vec<_> = registry.supported_models().collect();
        assert!(!supported.is_empty());
        // Registry carries models for APIs without providers; they must be
        // excluded from the supported view.
        assert!(supported.len() < total);
        assert!(
            supported.iter().all(|m| matches!(
                m.api,
                Api::AnthropicMessages | Api::OpenAiResponses | Api::OpenAiCompletions
            ))
        );
        // Unqualified find prefers a supported model on id collisions; with
        // OpenAiCompletions registered, the 302ai variant is supported and
        // appears first. Use a qualified lookup to pin the Anthropic model.
        let opus = registry.find("anthropic/claude-opus-4-7").unwrap();
        assert_eq!(opus.provider, "anthropic");
    }

    #[test]
    fn openai_completions_provider_is_registered() {
        let registry = Registry::builtin();
        // Any openai-completions model should now be supported.
        let nw = registry.first_for_provider("neuralwatt").unwrap();
        assert_eq!(nw.api, Api::OpenAiCompletions);
        assert!(registry.is_supported(nw));
    }

    #[test]
    fn thinking_value_clamps() {
        let registry = Registry::builtin();
        let opus = registry.find("anthropic/claude-opus-4-7").unwrap();
        assert_eq!(
            opus.thinking_value(crate::types::ThinkingLevel::Medium),
            Some("8192".to_owned())
        );
        let gpt = registry.find("openai/gpt-5.1").unwrap();
        // xhigh unsupported on gpt-5.1 -> clamps down to high
        assert_eq!(
            gpt.thinking_value(crate::types::ThinkingLevel::XHigh),
            Some("high".to_owned())
        );
        assert_eq!(gpt.thinking_value(crate::types::ThinkingLevel::Off), None);
    }
}
