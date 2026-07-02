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
            .unwrap_or_else(|e| panic!("embedded models.json failed to parse: {e}"));
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
        providers.insert(
            Api::CursorNative,
            Arc::new(crate::providers::cursor::CursorProvider::new()),
        );
        Self { models, providers }
    }

    pub fn models(&self) -> &[Model] {
        &self.models
    }

    /// A copy of this registry with `extra` models appended, sharing the same
    /// provider implementations. This is how extension-registered providers
    /// (`register_provider`) join the embedded catalog: the host converts
    /// their model configs and grafts them on per session.
    pub fn extended(&self, extra: Vec<Model>) -> Self {
        let mut models = self.models.clone();
        models.extend(extra);
        Self {
            models,
            providers: self.providers.clone(),
        }
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

    fn test_model(provider: &str, id: &str, api: Api) -> Model {
        Model {
            id: id.to_owned(),
            name: id.to_owned(),
            api,
            provider: provider.to_owned(),
            base_url: "https://example.com".to_owned(),
            reasoning: false,
            input: vec![],
            cost: Default::default(),
            context_window: 128_000,
            max_tokens: 8_192,
            headers: Default::default(),
            thinking_levels: None,
            adaptive_thinking: false,
        }
    }

    #[test]
    fn embedded_catalog_is_first_party_and_fully_supported() {
        let registry = Registry::builtin();
        // The embedded catalog carries only first-party providers Phi has
        // auth flows for; everything else arrives via `register_provider`.
        assert!(
            registry
                .models()
                .iter()
                .all(|m| matches!(m.provider.as_str(), "anthropic" | "openai" | "cursor"))
        );
        assert!(registry.models().iter().all(|m| registry.is_supported(m)));
    }

    #[test]
    fn openai_completions_provider_is_registered() {
        // No embedded model uses the generic OpenAI-completions protocol,
        // but the backend must stay registered: it's the default API for
        // extension-registered providers.
        let registry = Registry::builtin();
        let model = test_model("some-router", "some-model", Api::OpenAiCompletions);
        assert!(registry.is_supported(&model));
    }

    #[test]
    fn extended_registry_grafts_extension_models() {
        let registry = Registry::builtin();
        let base_total = registry.models().len();
        let extended = registry.extended(vec![test_model(
            "my-router",
            "my-model",
            Api::OpenAiCompletions,
        )]);
        assert_eq!(extended.models().len(), base_total + 1);
        let found = extended.find("my-router/my-model").unwrap();
        assert!(extended.is_supported(found));
        assert!(extended.find("anthropic/claude-opus-4-7").is_some());
        // The base registry is untouched.
        assert!(registry.find("my-router/my-model").is_none());
    }

    #[test]
    fn thinking_value_fallback_no_levels() {
        // A reasoning model with no thinking_levels should still return a
        // thinking value via the default fallback (the fix for the GLM bug
        // where thinking was silently dropped).
        let budget_model = Model {
            id: "test/glm".to_owned(),
            name: "Test GLM".to_owned(),
            api: Api::AnthropicMessages,
            provider: "test".to_owned(),
            base_url: "https://example.com".to_owned(),
            reasoning: true,
            input: vec![],
            cost: Default::default(),
            context_window: 128_000,
            max_tokens: 16_384,
            headers: Default::default(),
            thinking_levels: None,
            adaptive_thinking: false,
        };
        // Budget-based fallback: returns a token budget string.
        assert_eq!(
            budget_model.thinking_value(crate::types::ThinkingLevel::High),
            Some("16384".to_owned())
        );
        assert_eq!(
            budget_model.thinking_value(crate::types::ThinkingLevel::XHigh),
            Some("32768".to_owned())
        );
        assert_eq!(
            budget_model.thinking_value(crate::types::ThinkingLevel::Off),
            None
        );

        let adaptive_model = Model {
            id: "test/adaptive".to_owned(),
            name: "Test Adaptive".to_owned(),
            api: Api::AnthropicMessages,
            provider: "test".to_owned(),
            base_url: "https://example.com".to_owned(),
            reasoning: true,
            input: vec![],
            cost: Default::default(),
            context_window: 128_000,
            max_tokens: 16_384,
            headers: Default::default(),
            thinking_levels: None,
            adaptive_thinking: true,
        };
        // Adaptive fallback: returns the level name as effort string.
        assert_eq!(
            adaptive_model.thinking_value(crate::types::ThinkingLevel::High),
            Some("high".to_owned())
        );

        let openai_model = Model {
            id: "test/oai".to_owned(),
            name: "Test OAI".to_owned(),
            api: Api::OpenAiResponses,
            provider: "test".to_owned(),
            base_url: "https://example.com".to_owned(),
            reasoning: true,
            input: vec![],
            cost: Default::default(),
            context_window: 128_000,
            max_tokens: 16_384,
            headers: Default::default(),
            thinking_levels: None,
            adaptive_thinking: false,
        };
        // OpenAI Responses fallback: returns the level name as effort string.
        assert_eq!(
            openai_model.thinking_value(crate::types::ThinkingLevel::XHigh),
            Some("xhigh".to_owned())
        );

        // Non-reasoning model: always None, even with a thinking level set.
        let non_reasoning = Model {
            id: "test/no-reasoning".to_owned(),
            name: "Test No Reasoning".to_owned(),
            api: Api::AnthropicMessages,
            provider: "test".to_owned(),
            base_url: "https://example.com".to_owned(),
            reasoning: false,
            input: vec![],
            cost: Default::default(),
            context_window: 128_000,
            max_tokens: 16_384,
            headers: Default::default(),
            thinking_levels: None,
            adaptive_thinking: false,
        };
        assert_eq!(
            non_reasoning.thinking_value(crate::types::ThinkingLevel::High),
            None
        );
    }

    #[test]
    fn thinking_value_clamps() {
        let registry = Registry::builtin();
        // opus-4-7 is an adaptive thinking model — thinking_value returns
        // effort strings (low/medium/high/xhigh), not token budgets.
        let opus = registry.find("anthropic/claude-opus-4-7").unwrap();
        assert!(opus.adaptive_thinking, "opus-4-7 should be adaptive");
        assert_eq!(
            opus.thinking_value(crate::types::ThinkingLevel::Medium),
            Some("medium".to_owned())
        );
        assert_eq!(
            opus.thinking_value(crate::types::ThinkingLevel::XHigh),
            Some("xhigh".to_owned())
        );
        // opus-4-6 is adaptive with xhigh -> "max"
        let opus46 = registry.find("anthropic/claude-opus-4-6").unwrap();
        assert_eq!(
            opus46.thinking_value(crate::types::ThinkingLevel::XHigh),
            Some("max".to_owned())
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
