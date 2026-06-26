//! Steer / follow-up / next-turn queues.
//!
//! Three FIFO queues stored on [`crate::PhiHarness`] behind `tokio::sync::Mutex`:
//! - **steer**: mid-run guidance, drained at every save point and injected as
//!   system messages into the next iteration. Cleared by abort.
//! - **follow_up**: next user message after the current run completes. Cleared
//!   by abort.
//! - **next_turn**: messages prepended before the NEXT user-initiated prompt.
//!   Survives abort.

/// Source queue of a [`QueuedMessage`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueueSource {
    Steer,
    FollowUp,
    NextTurn,
}

/// One enqueued message with provenance.
#[derive(Clone, Debug)]
pub struct QueuedMessage {
    pub id: String,
    pub text: String,
    pub source: QueueSource,
}

/// The three harness queues.
#[derive(Default)]
pub struct Queues {
    steer: tokio::sync::Mutex<Vec<QueuedMessage>>,
    follow_up: tokio::sync::Mutex<Vec<QueuedMessage>>,
    next_turn: tokio::sync::Mutex<Vec<QueuedMessage>>,
}

impl Queues {
    /// Construct empty queues.
    pub fn new() -> Self {
        Self::default()
    }

    fn new_id() -> String {
        uuid::Uuid::now_v7().to_string()
    }

    /// Enqueue a steer message; returns its id.
    pub async fn enqueue_steer(&self, text: &str) -> String {
        let id = Self::new_id();
        self.steer.lock().await.push(QueuedMessage {
            id: id.clone(),
            text: text.to_owned(),
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
