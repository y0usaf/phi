//! Live (non-snapshotted) harness configuration.

use std::sync::Arc;

use phi_ai::ThinkingLevel;
use phi_ext::ToolSpec;
use phi_rlm::{LanguageModel, RlmConfig};

/// Harness configuration shared across turns. Updated by `set_model`,
/// `set_thinking`, `set_tools`; read when creating a fresh [`TurnState`].
pub struct HarnessConfig {
    /// Root model for completions.
    pub root_model: Arc<dyn LanguageModel>,
    /// Sub model for sub-calls.
    pub sub_model: Arc<dyn LanguageModel>,
    /// Thinking level override.
    pub thinking: Option<ThinkingLevel>,
    /// Base RLM config (limits, timeouts, etc).
    pub config: RlmConfig,
    /// Custom system prompt override.
    pub system_prompt: Option<String>,
    /// Active tool specs.
    pub tools: Vec<ToolSpec>,
    /// Active tool names.
    pub active_tools: Vec<String>,
}

impl HarnessConfig {
    /// Construct from the models and base config.
    pub fn new(
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
