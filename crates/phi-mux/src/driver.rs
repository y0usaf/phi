//! Per-session glue: spawns one embedded TUI session as a tokio task whose
//! terminal output lands in an in-memory vt100 screen, and tees its engine
//! events into sidebar status updates.

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use phi_tui::crossterm::event::Event;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::task::JoinHandle;

use crate::status::{self as status, SessionStatus};
use phi_session_tui::session_builder::{self, SharedResources};
use phi_session_tui::tui::{self, HostModalRequest, SessionIo, SuspendRequest};

/// Mux-local session identity, stable across rescans and restarts.
pub type SessionKey = u64;

/// Lines of scrollback each embedded vt100 screen keeps.
const SCROLLBACK: usize = 5000;

/// A running embedded session.
pub struct SessionHandle {
    /// Input events forwarded to the session's loop.
    pub input_tx: UnboundedSender<Event>,
    /// The session's screen; the mux composites the focused one.
    pub parser: Arc<Mutex<vt100::Parser>>,
    /// The session loop; exit/panic is reported on the status channel.
    pub task: JoinHandle<()>,
    /// JSONL backing this session (may not exist yet for a fresh draft).
    pub store_path: std::path::PathBuf,
    /// Last watch state told to the session (synthesized FocusGained/
    /// FocusLost). The session tui starts watched, matching this default;
    /// [`MuxApp::sync_watch_states`] reconciles it every loop iteration so
    /// unwatched sessions pause their idle animations.
    pub watched: std::cell::Cell<bool>,
}

impl SessionHandle {
    pub fn resize(&self, width: u16, height: u16) {
        lock_parser(&self.parser)
            .screen_mut()
            .set_size(height, width);
        let _ = self.input_tx.send(Event::Resize(width, height));
    }
}

/// Lock a session's parser, recovering from poison: a panicking session may
/// die mid-write, and the mux must keep compositing the last good screen.
pub fn lock_parser(parser: &Mutex<vt100::Parser>) -> std::sync::MutexGuard<'_, vt100::Parser> {
    parser
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Writer handed to an embedded session's `Terminal`: bytes go into the
/// session's vt100 screen, then the mux is nudged to recomposite.
pub struct ParserWriter {
    parser: Arc<Mutex<vt100::Parser>>,
    redraw_tx: UnboundedSender<()>,
}

impl std::io::Write for ParserWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        lock_parser(&self.parser).process(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let _ = self.redraw_tx.send(());
        Ok(())
    }
}

/// Channels a spawned session reports into, shared by all sessions.
#[derive(Clone)]
pub struct MuxChannels {
    pub status_tx: UnboundedSender<(SessionKey, SessionStatus)>,
    pub redraw_tx: UnboundedSender<()>,
    pub suspend_tx: UnboundedSender<(SessionKey, SuspendRequest)>,
    pub host_modal_tx: UnboundedSender<(SessionKey, HostModalRequest)>,
}

/// Build and launch one embedded session for `cwd` (optionally resuming an
/// existing JSONL), sized to the mux's terminal pane.
pub async fn spawn(
    key: SessionKey,
    cwd: &Path,
    resume: Option<&Path>,
    size: (u16, u16),
    shared: &SharedResources,
    channels: &MuxChannels,
    terminal_colors: phi_tui::TerminalColors,
) -> Result<SessionHandle> {
    let mut setup = session_builder::build_session(cwd, shared, resume).await?;

    // Tee engine events: the session Ui sees them unchanged, while depth-0
    // ones become sidebar status updates.
    let (ui_tx, ui_rx) = mpsc::unbounded_channel();
    let mut engine_rx = std::mem::replace(&mut setup.events, ui_rx);
    let status_tx = channels.status_tx.clone();
    tokio::spawn(async move {
        while let Some(event) = engine_rx.recv().await {
            if let Some(new_status) = status::status_for_event(&event) {
                let _ = status_tx.send((key, new_status));
            }
            if ui_tx.send(event).is_err() {
                break;
            }
        }
    });

    // Tag suspend requests with this session's key for the mux.
    let (sess_suspend_tx, mut sess_suspend_rx) = mpsc::unbounded_channel();
    let suspend_relay = channels.suspend_tx.clone();
    tokio::spawn(async move {
        while let Some(request) = sess_suspend_rx.recv().await {
            if suspend_relay.send((key, request)).is_err() {
                break;
            }
        }
    });

    let (sess_host_modal_tx, mut sess_host_modal_rx) = mpsc::unbounded_channel();
    let host_modal_relay = channels.host_modal_tx.clone();
    tokio::spawn(async move {
        while let Some(request) = sess_host_modal_rx.recv().await {
            if host_modal_relay.send((key, request)).is_err() {
                break;
            }
        }
    });

    let (width, height) = size;
    let parser = Arc::new(Mutex::new(vt100::Parser::new(height, width, SCROLLBACK)));
    let (input_tx, input_rx) = mpsc::unbounded_channel();
    let store_path = setup.ui.store.path().to_path_buf();
    let io = SessionIo {
        writer: Some(Box::new(ParserWriter {
            parser: parser.clone(),
            redraw_tx: channels.redraw_tx.clone(),
        })),
        event_rx: Some(input_rx),
        dimensions: Some(size),
        embedded: true,
        suspend_tx: Some(sess_suspend_tx),
        host_modal_tx: Some(sess_host_modal_tx),
        cwd: cwd.to_path_buf(),
        terminal_colors: Some(terminal_colors),
    };
    // Run the session under its own unwind boundary so one panicking
    // session marks itself crashed instead of taking the mux down.
    let exit_tx = channels.status_tx.clone();
    let redraw_on_exit = channels.redraw_tx.clone();
    let task = tokio::spawn(async move {
        use futures::FutureExt as _;
        let result = std::panic::AssertUnwindSafe(tui::run(setup, io))
            .catch_unwind()
            .await;
        let status = match result {
            Ok(Ok(())) => SessionStatus::Done,
            Ok(Err(_)) | Err(_) => SessionStatus::Crashed,
        };
        let _ = exit_tx.send((key, status));
        let _ = redraw_on_exit.send(());
    });
    Ok(SessionHandle {
        input_tx,
        parser,
        task,
        store_path,
        watched: std::cell::Cell::new(true),
    })
}

/// Drain any pending terminal events (key, mouse, resize, etc.) that may
/// have accumulated during a suspend or mode switch, discarding them so
/// they don't fire as phantom input on resume.
pub fn drain_pending_terminal_events() {
    use phi_tui::crossterm::event::{self};
    use std::time::Duration;

    while event::poll(Duration::from_millis(0)).unwrap_or(false) {
        if event::read().is_err() {
            break;
        }
    }
}
