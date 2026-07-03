//! Ask-the-user primitive for extensions (pi's `ctx.ui.confirm`/`select`
//! analog, WS7.2).
//!
//! Extensions capture a [`UiHandle`] at `register()` time via
//! [`crate::ExtensionHost::ui_handle`]. The handle is late-bound: it starts
//! disconnected, and an interactive host attaches a request channel via
//! [`crate::AppRuntime::connect_ui`] when its event loop starts (the session
//! TUI is built long after extensions register). Non-interactive modes
//! (`phi -p`) never connect, so [`UiHandle::has_ui`] is `false` there and
//! each extension chooses its own no-UI policy — an approval gate fails
//! closed, a cosmetic prompt is skipped.
//!
//! [`UiHandle::ask`] blocks the calling thread. That is deliberate: event
//! handlers run on the blocking pool (see `AppRuntime::dispatch`), so a
//! handler can wait for the user's answer while the host's event loop keeps
//! rendering. Callers must keep their `wait` under their registration's
//! `timeout` override — the dispatch watchdog stops waiting first otherwise,
//! and for interception gates a timed-out handler contributes nothing (the
//! tool proceeds).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;

/// One choice offered by a [`UiPrompt`].
#[derive(Clone, Debug)]
pub struct UiPromptOption {
    pub label: String,
    /// Optional secondary text shown alongside the label.
    pub detail: Option<String>,
}

impl UiPromptOption {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            detail: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// A question an extension asks the interactive host.
#[derive(Clone, Debug)]
pub struct UiPrompt {
    /// One-line question ("Allow this bash command?").
    pub title: String,
    /// Free-form context shown before the options (the command, code, or
    /// path being gated). May be multi-line; hosts truncate for display.
    pub body: String,
    pub options: Vec<UiPromptOption>,
}

/// A prompt in flight to the host, carrying its reply channel.
///
/// The reply sender is buffered (capacity 1) so the host's event loop never
/// blocks answering. Dropping the request without replying reads as
/// "dismissed" on the asking side — a host that quits or cancels a run can
/// simply drop queued requests.
pub struct UiPromptRequest {
    pub prompt: UiPrompt,
    pub reply: std::sync::mpsc::SyncSender<Option<usize>>,
}

/// Outcome of [`UiHandle::ask`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiAskOutcome {
    /// The user picked option `i` (index into `UiPrompt::options`).
    Answer(usize),
    /// The prompt was dismissed or cancelled without an answer.
    Dismissed,
    /// No interactive host is connected.
    NoUi,
    /// No answer arrived within the caller's `wait`.
    TimedOut,
}

/// Cloneable, late-bound channel to the interactive host.
///
/// `Default` yields a disconnected handle; the same handle instance is
/// shared between the registration host and the built `AppRuntime`, so a
/// host connecting after build reaches every extension that captured it.
#[derive(Clone, Default)]
pub struct UiHandle {
    tx: Arc<Mutex<Option<UnboundedSender<UiPromptRequest>>>>,
}

impl UiHandle {
    /// Attach the interactive host's request channel.
    pub fn connect(&self, tx: UnboundedSender<UiPromptRequest>) {
        *self.tx.lock().unwrap() = Some(tx);
    }

    /// Whether an interactive host is currently reachable.
    pub fn has_ui(&self) -> bool {
        matches!(&*self.tx.lock().unwrap(), Some(tx) if !tx.is_closed())
    }

    /// Ask the host `prompt`, blocking the calling thread up to `wait`.
    /// Intended for event handlers (already on the blocking pool); never
    /// call from an async context.
    pub fn ask(&self, prompt: UiPrompt, wait: Duration) -> UiAskOutcome {
        let Some(tx) = self.tx.lock().unwrap().clone() else {
            return UiAskOutcome::NoUi;
        };
        let (reply_tx, reply_rx) = std::sync::mpsc::sync_channel(1);
        if tx
            .send(UiPromptRequest {
                prompt,
                reply: reply_tx,
            })
            .is_err()
        {
            return UiAskOutcome::NoUi;
        }
        match reply_rx.recv_timeout(wait) {
            Ok(Some(i)) => UiAskOutcome::Answer(i),
            Ok(None) => UiAskOutcome::Dismissed,
            // The host dropped the request without answering (run cancelled,
            // TUI shut down): indistinguishable from an explicit dismissal.
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => UiAskOutcome::Dismissed,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => UiAskOutcome::TimedOut,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt() -> UiPrompt {
        UiPrompt {
            title: "Allow?".to_owned(),
            body: "rm -rf /tmp/x".to_owned(),
            options: vec![UiPromptOption::new("Allow"), UiPromptOption::new("Deny")],
        }
    }

    #[test]
    fn disconnected_handle_reports_no_ui() {
        let handle = UiHandle::default();
        assert!(!handle.has_ui());
        assert_eq!(
            handle.ask(prompt(), Duration::from_millis(10)),
            UiAskOutcome::NoUi
        );
    }

    #[tokio::test]
    async fn answer_round_trips() {
        let handle = UiHandle::default();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        handle.connect(tx);
        assert!(handle.has_ui());

        let asker = handle.clone();
        let ask = tokio::task::spawn_blocking(move || asker.ask(prompt(), Duration::from_secs(5)));
        let req = rx.recv().await.expect("request arrives");
        assert_eq!(req.prompt.options.len(), 2);
        req.reply.send(Some(1)).expect("buffered reply");
        assert_eq!(ask.await.unwrap(), UiAskOutcome::Answer(1));
    }

    #[tokio::test]
    async fn dropped_request_reads_as_dismissed() {
        let handle = UiHandle::default();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        handle.connect(tx);

        let asker = handle.clone();
        let ask = tokio::task::spawn_blocking(move || asker.ask(prompt(), Duration::from_secs(5)));
        let req = rx.recv().await.expect("request arrives");
        drop(req);
        assert_eq!(ask.await.unwrap(), UiAskOutcome::Dismissed);
    }

    #[tokio::test]
    async fn unanswered_ask_times_out() {
        let handle = UiHandle::default();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        handle.connect(tx);

        let asker = handle.clone();
        let ask =
            tokio::task::spawn_blocking(move || asker.ask(prompt(), Duration::from_millis(50)));
        // Receive but never answer; keep the request alive past the wait.
        let req = rx.recv().await.expect("request arrives");
        assert_eq!(ask.await.unwrap(), UiAskOutcome::TimedOut);
        drop(req);
    }

    #[test]
    fn closed_host_channel_reports_no_ui() {
        let handle = UiHandle::default();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        handle.connect(tx);
        drop(rx);
        assert!(!handle.has_ui());
        assert_eq!(
            handle.ask(prompt(), Duration::from_millis(10)),
            UiAskOutcome::NoUi
        );
    }
}
