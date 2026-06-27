//! Live (non-snapshotted) harness configuration.

use std::sync::Arc;

use crate::config::RlmConfig;
use crate::lm_core::LanguageModel;
use phi_ai::ThinkingLevel;
use phi_ext::ToolSpec;

/// Harness configuration shared across turns. Updated by `set_model`,
/// `set_thinking`, `set_tools`; read when creating a fresh [`TurnState`](crate::engine::TurnState).
#[allow(dead_code)]
pub(crate) struct HarnessConfig {
    /// Root model for completions.
    pub(crate) root_model: Arc<dyn LanguageModel>,
    /// Sub model for sub-calls.
    pub(crate) sub_model: Arc<dyn LanguageModel>,
    /// Thinking level override.
    pub(crate) thinking: Option<ThinkingLevel>,
    /// Base RLM config (limits, timeouts, etc).
    pub(crate) config: RlmConfig,
    /// Custom system prompt override.
    pub(crate) system_prompt: Option<String>,
    /// Active tool specs.
    pub(crate) tools: Vec<ToolSpec>,
    /// Active tool names.
    pub(crate) active_tools: Vec<String>,
}

impl HarnessConfig {
    /// Construct from the models and base config.
    pub(crate) fn new(
        root_model: Arc<dyn LanguageModel>,
        sub_model: Arc<dyn LanguageModel>,
        config: RlmConfig,
    ) -> Self {
        Self {
            root_model,
            sub_model,
            thinking: None,
            config,
            system_prompt: None,
            tools: Vec::new(),
            active_tools: Vec::new(),
        }
    }
}
