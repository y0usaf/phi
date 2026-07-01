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
        assert!(supported.iter().all(|m| matches!(
            m.api,
            Api::AnthropicMessages
                | Api::OpenAiResponses
                | Api::OpenAiCompletions
                | Api::CursorNative
        )));
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
