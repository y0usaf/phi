mod builder;
mod constants;
mod metadata;

#[cfg(test)]
mod tests;

pub use builder::{Skill, SystemPromptOptions, build_system_prompt};
pub use constants::DEFAULT_ANSWER_NUDGE;
pub(crate) use constants::{PROSE_NUDGE, STRICT_PROSE_NUDGE};

// Test-only re-exports.
#[cfg(test)]
pub(crate) use builder::ContextFile;
#[cfg(test)]
pub(crate) use constants::{ORCHESTRATOR_ADDENDUM, TURN_SCARCITY_NUDGE};

// Internal-only re-exports (accessed via crate::prompts:: within this crate).
pub(crate) use builder::build_user_prompt;
pub(crate) use metadata::{QueryMetadata, metadata_user_prompt};
