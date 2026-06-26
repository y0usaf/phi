//! Save points: reconcile state between RLM iterations.
//!
//! A save point is inserted after each assistant turn + REPL execution
//! completes, before the next provider request. At the save point the
//! harness flushes pending session writes, drains steer/follow-up queues,
//! and creates a fresh [`TurnState`] snapshot from the live harness config
//! so mid-run changes take effect on the next iteration.

use crate::PhiHarness;
use crate::queue::QueuedMessage;
use phi_rlm::ChatMessage;

/// A save point bound to a harness.
pub struct SavePoint<'a> {
    harness: &'a PhiHarness,
}

impl<'a> SavePoint<'a> {
    /// Construct a save point for the given harness.
    pub fn new(harness: &'a PhiHarness) -> Self {
        Self { harness }
    }

    /// Run the save point: drain queues and return messages to inject
    /// into the next iteration's history. Also flushes any pending
    /// durable writes.
    pub async fn run(&self) -> Vec<QueuedMessage> {
        self.harness.queues().drain_at_save_point().await
    }

    /// Run the save point and format drained queue messages as a single
    /// [`ChatMessage`] (user role) suitable for injection into history.
    /// Returns `None` if no messages were drained.
    pub async fn run_as_message(&self) -> Option<ChatMessage> {
        let drained = self.run().await;
        if drained.is_empty() {
            return None;
        }
        let combined = drained
            .into_iter()
            .map(|m| {
                let label = match m.source {
                    crate::queue::QueueSource::Steer => "Steer",
                    crate::queue::QueueSource::FollowUp => "Follow-up",
                    crate::queue::QueueSource::NextTurn => "Next-turn",
                };
                format!("{label}: {}", m.text)
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        Some(ChatMessage {
            role: phi_rlm::Role::User,
            content: combined,
            ..Default::default()
        })
    }

}
