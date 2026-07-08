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
    /// The bare registry: every API protocol implementation, zero models.
    /// There is no embedded model catalog — the first-party catalog is
    /// `phi-app`'s `builtins/providers.lua`, registered through the same
    /// public `phi.register_provider` door user extensions use and grafted
    /// on via [`Registry::extended`] (DESIGN.md, The Rule: built-ins are
    /// extensions).
    pub fn builtin() -> Self {
        let models: Vec<Model> = Vec::new();
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
    /// Prefers supported models when ids collide across providers. If the
    /// query contains slashes, first try an exact `provider/id` match, then
    /// fall back to a bare-id lookup so first-party providers can expose ids
    /// like `vendor/model` without forcing full qualification.
    pub fn find(&self, query: &str) -> Option<&Model> {
        if let Some((provider, id)) = query.split_once('/')
            && let Some(found) = self
                .models
                .iter()
                .find(|m| m.provider == provider && m.id == id)
        {
            return Some(found);
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
            reasoning_control: None,
        }
    }

    #[test]
    fn builtin_registry_is_bare_protocols_no_models() {
        // The Rule: the catalog is builtins/providers.lua in phi-app,
        // registered via phi.register_provider. The bare registry carries
        // only the protocol implementations — every API is driveable, no
        // model is hardcoded.
        let registry = Registry::builtin();
        assert!(registry.models().is_empty());
        for api in [
            Api::AnthropicMessages,
            Api::OpenAiResponses,
            Api::OpenAiCompletions,
            Api::CursorNative,
        ] {
            let model = test_model("any", "any-model", api);
            assert!(registry.is_supported(&model), "{api:?} must be registered");
        }
        assert!(registry.find("nope").is_none());
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
            reasoning_control: None,
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
            reasoning_control: None,
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
            reasoning_control: None,
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
            reasoning_control: None,
        };
        assert_eq!(
            non_reasoning.thinking_value(crate::types::ThinkingLevel::High),
            None
        );
    }

    #[test]
    fn thinking_value_clamps() {
        // An adaptive-thinking model returns effort strings, xhigh mapping
        // to whatever the level map declares (catalog: opus-4-6 → "max").
        let mut adaptive = test_model("anthropic", "adaptive-model", Api::AnthropicMessages);
        adaptive.reasoning = true;
        adaptive.adaptive_thinking = true;
        adaptive.thinking_levels = Some(
            [
                (crate::types::ThinkingLevel::Medium, Some("medium".into())),
                (crate::types::ThinkingLevel::High, Some("high".into())),
                (crate::types::ThinkingLevel::XHigh, Some("max".into())),
            ]
            .into_iter()
            .collect(),
        );
        assert_eq!(
            adaptive.thinking_value(crate::types::ThinkingLevel::Medium),
            Some("medium".to_owned())
        );
        assert_eq!(
            adaptive.thinking_value(crate::types::ThinkingLevel::XHigh),
            Some("max".to_owned())
        );

        // A level absent from the map clamps down to the nearest supported
        // one (catalog: gpt-5.1 has no xhigh → clamps to high).
        let mut capped = test_model("openai", "capped-model", Api::OpenAiResponses);
        capped.reasoning = true;
        capped.thinking_levels = Some(
            [
                (crate::types::ThinkingLevel::Low, Some("low".into())),
                (crate::types::ThinkingLevel::Medium, Some("medium".into())),
                (crate::types::ThinkingLevel::High, Some("high".into())),
            ]
            .into_iter()
            .collect(),
        );
        assert_eq!(
            capped.thinking_value(crate::types::ThinkingLevel::XHigh),
            Some("high".to_owned())
        );
        assert_eq!(
            capped.thinking_value(crate::types::ThinkingLevel::Off),
            None
        );
    }
}
