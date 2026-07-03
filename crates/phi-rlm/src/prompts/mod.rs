mod builder;
mod constants;
mod metadata;
mod overrides;

#[cfg(test)]
mod tests;

pub use builder::{SystemPromptOptions, build_system_prompt};
pub use constants::DEFAULT_ANSWER_NUDGE;
pub(crate) use constants::{
    COMPACTION_SUMMARY_PROMPT, EMPTY_TURN_NUDGE, compaction_continue_message,
};
pub use overrides::discover_prompt_overrides;

// Test-only re-exports.
#[cfg(test)]
pub(crate) use builder::ContextFile;
#[cfg(test)]
pub(crate) use constants::{ORCHESTRATOR_ADDENDUM, TURN_SCARCITY_NUDGE};

// Internal-only re-exports (accessed via crate::prompts:: within this crate).
pub(crate) use builder::session_notes;
pub(crate) use metadata::{QueryMetadata, metadata_user_prompt};
