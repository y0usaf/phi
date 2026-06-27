//! Steer / follow-up / next-turn queues.
//!
//! Three FIFO queues stored on [`super::PhiHarness`] behind `tokio::sync::Mutex`:
//! - **steer**: mid-run guidance, drained at every save point and injected as
//!   system messages into the next iteration. Cleared by abort.
//! - **follow_up**: next user message after the current run completes. Cleared
//!   by abort.
//! - **next_turn**: messages prepended before the NEXT user-initiated prompt.
//!   Survives abort.

/// Source queue of a [`QueuedMessage`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)]
pub(crate) enum QueueSource {
    Steer,
    FollowUp,
    NextTurn,
}

/// One enqueued message with provenance.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) struct QueuedMessage {
    pub(crate) id: String,
    pub(crate) text: String,
    pub(crate) source: QueueSource,
}

/// The three harness queues.
#[derive(Default)]
#[allow(dead_code)]
pub(crate) struct Queues {
    steer: tokio::sync::Mutex<Vec<QueuedMessage>>,
    follow_up: tokio::sync::Mutex<Vec<QueuedMessage>>,
    next_turn: tokio::sync::Mutex<Vec<QueuedMessage>>,
}

#[allow(dead_code)]
impl Queues {
    /// Construct empty queues.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn new_id() -> String {
        uuid::Uuid::now_v7().to_string()
    }

    /// Enqueue a steer message; returns its id.
    pub(crate) async fn enqueue_steer(&self, text: &str) -> String {
        let id = Self::new_id();
        self.steer.lock().await.push(QueuedMessage {
            id: id.clone(),
            text: text.to_owned(),
            source: QueueSource::Steer,
        });
        id
    }

    /// Enqueue a follow-up message; returns its id.
    pub(crate) async fn enqueue_follow_up(&self, text: &str) -> String {
        let id = Self::new_id();
        self.follow_up.lock().await.push(QueuedMessage {
            id: id.clone(),
            text: text.to_owned(),
            source: QueueSource::FollowUp,
        });
        id
    }

    /// Enqueue a next-turn message; returns its id.
    pub(crate) async fn enqueue_next_turn(&self, text: &str) -> String {
        let id = Self::new_id();
        self.next_turn.lock().await.push(QueuedMessage {
            id: id.clone(),
            text: text.to_owned(),
            source: QueueSource::NextTurn,
        });
        id
    }

    /// Drain steer + follow-up at a save point. Does NOT drain next_turn.
    pub(crate) async fn drain_at_save_point(&self) -> Vec<QueuedMessage> {
        let mut out = Vec::new();
        out.extend(self.steer.lock().await.drain(..));
        out.extend(self.follow_up.lock().await.drain(..));
        out
    }

    /// Drain next_turn at the start of the next user-initiated prompt.
    pub(crate) async fn drain_next_turn(&self) -> Vec<QueuedMessage> {
        self.next_turn.lock().await.drain(..).collect()
    }

    /// Clear steer + follow-up (used by abort). Does NOT clear next_turn.
    pub(crate) async fn clear_runtime_queues(&self) {
        self.steer.lock().await.clear();
        self.follow_up.lock().await.clear();
    }

    /// Snapshot the steer queue (for recovery / inspection).
    pub(crate) async fn steer_snapshot(&self) -> Vec<QueuedMessage> {
        self.steer.lock().await.clone()
    }

    /// Snapshot the follow-up queue.
    pub(crate) async fn follow_up_snapshot(&self) -> Vec<QueuedMessage> {
        self.follow_up.lock().await.clone()
    }

    /// Snapshot the next-turn queue.
    pub(crate) async fn next_turn_snapshot(&self) -> Vec<QueuedMessage> {
        self.next_turn.lock().await.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::{QueueSource, Queues};

    #[tokio::test]
    async fn queue_steer_enqueue_and_drain() {
        let q = Queues::new();
        let id1 = q.enqueue_steer("go left").await;
        let id2 = q.enqueue_steer("go right").await;
        assert!(!id1.is_empty());
        assert!(!id2.is_empty());

        let drained = q.drain_at_save_point().await;
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].text, "go left");
        assert_eq!(drained[1].text, "go right");
        assert_eq!(drained[0].source, QueueSource::Steer);

        // Drain again: should be empty.
        let drained2 = q.drain_at_save_point().await;
        assert!(drained2.is_empty());
    }

    #[tokio::test]
    async fn queue_follow_up_and_steer_drained_together() {
        let q = Queues::new();
        q.enqueue_steer("steer1").await;
        q.enqueue_follow_up("follow1").await;

        let drained = q.drain_at_save_point().await;
        assert_eq!(drained.len(), 2);
        // Steer comes first, then follow-up.
        assert_eq!(drained[0].source, QueueSource::Steer);
        assert_eq!(drained[1].source, QueueSource::FollowUp);
    }

    #[tokio::test]
    async fn queue_next_turn_survives_clear_runtime() {
        let q = Queues::new();
        q.enqueue_steer("steer").await;
        q.enqueue_next_turn("next").await;

        q.clear_runtime_queues().await;

        // Steer should be cleared, next_turn should survive.
        let steer_snap = q.steer_snapshot().await;
        assert!(steer_snap.is_empty());

        let next = q.drain_next_turn().await;
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].text, "next");
        assert_eq!(next[0].source, QueueSource::NextTurn);
    }
}
