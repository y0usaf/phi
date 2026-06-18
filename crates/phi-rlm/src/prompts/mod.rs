//! Pi-style composable system-prompt builder for RLM.
//!
//! The monolithic `RLM_SYSTEM_PROMPT` template has been decomposed into named,
//! overridable sections assembled by `SystemPromptOptions` + `build_system_prompt`.

mod builder;
mod constants;
mod metadata;
mod tests;

pub use builder::{ContextFile, Skill, SystemPromptOptions, build_system_prompt, build_user_prompt};
pub use constants::{
    DEFAULT_ANSWER_NUDGE, DEFAULT_IDENTITY, DEFAULT_MECHANICS, DEFAULT_REPL_TOOLS,
    ORCHESTRATOR_ADDENDUM, PROSE_NUDGE, TURN_SCARCITY_NUDGE,
};
pub use metadata::{QueryMetadata, metadata_user_prompt};
