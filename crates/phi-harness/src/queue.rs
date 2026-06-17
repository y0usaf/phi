//! Steer / follow-up / next-turn queues.
//!
//! Three FIFO queues stored on [`crate::PhiHarness`] behind `tokio::sync::Mutex`:
//! - **steer**: mid-run guidance, drained at every save point and injected as
//!   system messages into the next iteration. Cleared by abort.
//! - **follow_up**: next user message after the current run completes. Cleared
//!   by abort.
//! - **next_turn**: messages prepended before the NEXT user-initiated prompt.
//!   Survives abort.

use jiff::Timestamp;

/// Source queue of a [`QueuedMessage`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueueSource {
    Steer,
    FollowUp,
    NextTurn,
}

/// One enqueued message with provenance and timestamp.
#[derive(Clone, Debug)]
pub struct QueuedMessage {
    pub id: String,
    pub text: String,
    pub timestamp: Timestamp,
    pub source: QueueSource,
}

/// Queue drain mode (default `Interleave`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum QueueMode {
    /// Inject at the next save point (default).
    #[default]
    Interleave,
    /// Accumulate until the run ends, then inject as one message.
    Accumulate,
}

/// The three harness queues.
#[derive(Default)]
pub struct Queues {
    steer: tokio::sync::Mutex<Vec<QueuedMessage>>,
    follow_up: tokio::sync::Mutex<Vec<QueuedMessage>>,
    next_turn: tokio::sync::Mutex<Vec<QueuedMessage>>,
    steer_mode: tokio::sync::RwLock<QueueMode>,
    follow_up_mode: tokio::sync::RwLock<QueueMode>,
}

impl Queues {
    /// Construct empty queues with default (Interleave) modes.
    pub fn new() -> Self {
        Self::default()
    }

    fn new_id() -> String {
        uuid::Uuid::now_v7().to_string()
    }

    fn now() -> Timestamp {
        Timestamp::now()
    }

    /// Enqueue a steer message; returns its id.
    pub async fn enqueue_steer(&self, text: &str) -> String {
        let id = Self::new_id();
        self.steer.lock().await.push(QueuedMessage {
            id: id.clone(),
            text: text.to_owned(),
            timestamp: Self::now(),
            source: QueueSource::Steer,
        });
        id
    }

    /// Enqueue a follow-up message; returns its id.
    pub async fn enqueue_follow_up(&self, text: &str) -> String {
        let id = Self::new_id();
        self.follow_up.lock().await.push(QueuedMessage {
            id: id.clone(),
            text: text.to_owned(),
            timestamp: Self::now(),
            source: QueueSource::FollowUp,
        });
        id
    }

    /// Enqueue a next-turn message; returns its id.
    pub async fn enqueue_next_turn(&self, text: &str) -> String {
        let id = Self::new_id();
        self.next_turn.lock().await.push(QueuedMessage {
            id: id.clone(),
            text: text.to_owned(),
            timestamp: Self::now(),
            source: QueueSource::NextTurn,
        });
        id
    }

    /// Drain steer + follow-up at a save point. Does NOT drain next_turn.
    pub async fn drain_at_save_point(&self) -> Vec<QueuedMessage> {
        let mut out = Vec::new();
        out.extend(self.steer.lock().await.drain(..));
        out.extend(self.follow_up.lock().await.drain(..));
        out
    }

    /// Drain next_turn at the start of the next user-initiated prompt.
    pub async fn drain_next_turn(&self) -> Vec<QueuedMessage> {
        self.next_turn.lock().await.drain(..).collect()
    }

    /// Clear steer + follow-up (used by abort). Does NOT clear next_turn.
    pub async fn clear_runtime_queues(&self) {
        self.steer.lock().await.clear();
        self.follow_up.lock().await.clear();
    }

    /// Set the steer drain mode.
    pub async fn set_steer_mode(&self, mode: QueueMode) {
        *self.steer_mode.write().await = mode;
    }

    /// Set the follow-up drain mode.
    pub async fn set_follow_up_mode(&self, mode: QueueMode) {
        *self.follow_up_mode.write().await = mode;
    }

    /// Snapshot the steer queue (for recovery / inspection).
    pub async fn steer_snapshot(&self) -> Vec<QueuedMessage> {
        self.steer.lock().await.clone()
    }

    /// Snapshot the follow-up queue.
    pub async fn follow_up_snapshot(&self) -> Vec<QueuedMessage> {
        self.follow_up.lock().await.clone()
    }

    /// Snapshot the next-turn queue.
    pub async fn next_turn_snapshot(&self) -> Vec<QueuedMessage> {
        self.next_turn.lock().await.clone()
    }
}
