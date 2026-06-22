//! Pi-style composable system-prompt builder for RLM.
//!
//! The monolithic `RLM_SYSTEM_PROMPT` template has been decomposed into named,
//! overridable sections assembled by `SystemPromptOptions` + `build_system_prompt`.

mod builder;
mod constants;
mod metadata;

#[cfg(test)]
mod tests;

pub use builder::{
    ContextFile, Skill, SystemPromptOptions, build_system_prompt, build_user_prompt,
};
pub use constants::{
    DEFAULT_ANSWER_NUDGE, DEFAULT_ACTIONS, DEFAULT_CYBER_RISK, DEFAULT_DOING_TASKS,
    DEFAULT_IDENTITY, DEFAULT_MECHANICS, DEFAULT_OUTPUT_EFFICIENCY, DEFAULT_REPL_TOOLS,
    DEFAULT_SUMMARIZE_TOOL_RESULTS, DEFAULT_SYSTEM_REMINDER_SEMANTICS, DEFAULT_TONE_AND_STYLE,
    DEFAULT_USING_YOUR_TOOLS, ORCHESTRATOR_ADDENDUM, TURN_SCARCITY_NUDGE,
};
pub use metadata::{QueryMetadata, metadata_user_prompt};
