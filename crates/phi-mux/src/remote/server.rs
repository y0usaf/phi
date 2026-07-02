//! Daemon side: unix listener, per-client connection tasks, and the
//! registry that broadcasts rendered frames to every attached client.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use phi_tui::TerminalColors;
use phi_tui::crossterm::event::Event;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;

use super::daemon;
use super::protocol::{ClientMsg, ServerMsg, decode, encode};
use phi_session::config::MuxConfig;
use phi_session_tui::session_builder::SharedResources;

/// How long a freshly spawned daemon waits for its first client before
/// concluding the spawner died and exiting instead of lingering forever.
const FIRST_ATTACH_TIMEOUT: Duration = Duration::from_secs(60);

/// A client's `Attach` handshake payload.
#[derive(Debug)]
pub struct AttachInfo {
    pub cols: u16,
    pub rows: u16,
    pub cwd: PathBuf,
    pub projects: Vec<String>,
    pub terminal_colors: Option<TerminalColors>,
}

/// Fan-in events from all client connections into the mux main loop.
#[derive(Debug)]
pub enum RemoteEvent {
    Attached {
        id: u64,
        info: AttachInfo,
    },
    Detached {
        id: u64,
    },
    Input {
        id: u64,
        event: Event,
    },
    /// A client's size changed (the registry is already updated); the app
    /// recomputes the shared canvas size.
    Resized,
    EditResult {
        id: u64,
        result: Result<Option<String>, String>,
    },
    ClipboardText {
        id: u64,
        text: Option<String>,
    },
    Kill,
}

struct RemoteClient {
    tx: mpsc::UnboundedSender<ServerMsg>,
    cols: u16,
    rows: u16,
    cwd: PathBuf,
}

#[derive(Default)]
struct Inner {
    clients: HashMap<u64, RemoteClient>,
    next_id: u64,
}

/// Attached-client registry: the broadcast fanout and the source of truth
/// for the shared canvas size (min across attached clients).
#[derive(Default)]
pub struct ClientRegistry {
    inner: Mutex<Inner>,
}

impl ClientRegistry {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn register(
        &self,
        cols: u16,
        rows: u16,
        cwd: PathBuf,
        tx: mpsc::UnboundedSender<ServerMsg>,
    ) -> u64 {
        let mut inner = self.lock();
        inner.next_id += 1;
        let id = inner.next_id;
        inner.clients.insert(
            id,
            RemoteClient {
                tx,
                cols,
                rows,
                cwd,
            },
        );
        id
    }

    /// The working directory a client attached from (for resolving
    /// client-relative paths like `:open src`).
    pub fn client_cwd(&self, id: u64) -> Option<PathBuf> {
        self.lock().clients.get(&id).map(|c| c.cwd.clone())
    }

    fn set_size(&self, id: u64, cols: u16, rows: u16) {
        if let Some(client) = self.lock().clients.get_mut(&id) {
            client.cols = cols;
            client.rows = rows;
        }
    }

    fn remove(&self, id: u64) {
        self.lock().clients.remove(&id);
    }

    /// The shared canvas size: the smallest attached terminal in each
    /// dimension (tmux behavior — anything larger would paint off-screen on
    /// the smallest client). `None` with no clients attached.
    pub fn target_size(&self) -> Option<(u16, u16)> {
        let inner = self.lock();
        let cols = inner.clients.values().map(|c| c.cols).min()?;
        let rows = inner.clients.values().map(|c| c.rows).min()?;
        Some((cols, rows))
    }

    pub fn client_count(&self) -> usize {
        self.lock().clients.len()
    }

    /// Send `msg` to every attached client, pruning any whose write task
    /// has gone away.
    pub fn broadcast(&self, msg: &ServerMsg) {
        self.lock()
            .clients
            .retain(|_, client| client.tx.send(msg.clone()).is_ok());
    }

    /// Send `msg` to one client. Returns false when it isn't attached.
    pub fn send_to(&self, id: u64, msg: ServerMsg) -> bool {
        self.lock()
            .clients
            .get(&id)
            .is_some_and(|client| client.tx.send(msg).is_ok())
    }
}

/// Removal guarantee (zellij's route-thread invariant): however a
/// connection task ends — clean detach, EOF, decode error, panic — the
/// client leaves the registry and the app hears about it.
struct RemoveOnDrop {
    registry: Arc<ClientRegistry>,
    events_tx: mpsc::UnboundedSender<RemoteEvent>,
    id: u64,
}

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        self.registry.remove(self.id);
        let _ = self.events_tx.send(RemoteEvent::Detached { id: self.id });
    }
}

/// Run the mux daemon: bind the socket, accept clients, seed the app from
/// the first client's handshake, then run the shared `MuxApp` main loop.
pub async fn run(shared: SharedResources, mux_config: MuxConfig, cwd: PathBuf) -> Result<()> {
    let Some(lock) = daemon::try_acquire_daemon_lock()? else {
        bail!("another mux daemon is already running");
    };
    let socket = daemon::socket_path();
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Safe to unlink unconditionally: we hold the lock, so any existing
    // file is provably stale.
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket)
        .with_context(|| format!("bind mux socket {}", socket.display()))?;
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600));
    }

    let registry = Arc::new(ClientRegistry::default());
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    tokio::spawn(accept_loop(listener, registry.clone(), events_tx.clone()));

    // Graceful shutdown on SIGTERM/SIGHUP: route through the normal quit
    // path (main loop exits → state persists → clients get Exit). SIGINT is
    // deliberately ignored — a stray ^C must not kill every session.
    tokio::spawn({
        let events_tx = events_tx.clone();
        async move {
            use tokio::signal::unix::{SignalKind, signal};
            let (Ok(mut term), Ok(mut hup)) = (
                signal(SignalKind::terminate()),
                signal(SignalKind::hangup()),
            ) else {
                return;
            };
            tokio::select! {
                _ = term.recv() => {}
                _ = hup.recv() => {}
            }
            let _ = events_tx.send(RemoteEvent::Kill);
        }
    });
    daemon::signal_ready();

    // The first client's handshake seeds the canvas size and terminal
    // colors (the daemon has no tty to probe). Its cwd/projects were
    // inherited through our own spawn environment already.
    let first = tokio::time::timeout(FIRST_ATTACH_TIMEOUT, async {
        loop {
            match events_rx.recv().await {
                Some(RemoteEvent::Attached { id, info }) => break Some((id, info)),
                Some(RemoteEvent::Kill) | None => break None,
                Some(_) => continue,
            }
        }
    })
    .await;

    let result = match first {
        Ok(Some((_, seed))) => {
            crate::app::run_server(shared, mux_config, cwd, seed, registry.clone(), events_rx).await
        }
        // Killed before anyone attached: nothing to run.
        Ok(None) => Ok(()),
        Err(_) => Err(anyhow::anyhow!(
            "no client attached within {}s",
            FIRST_ATTACH_TIMEOUT.as_secs()
        )),
    };

    // Cleanup runs on every exit path so the socket never goes stale.
    registry.broadcast(&ServerMsg::Exit {
        reason: "mux quit".to_owned(),
    });
    let _ = std::fs::remove_file(&socket);
    drop(lock);
    result
}

async fn accept_loop(
    listener: UnixListener,
    registry: Arc<ClientRegistry>,
    events_tx: mpsc::UnboundedSender<RemoteEvent>,
) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                tokio::spawn(handle_connection(
                    stream,
                    registry.clone(),
                    events_tx.clone(),
                ));
            }
            Err(err) => {
                eprintln!("mux daemon: accept failed: {err}");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

async fn handle_connection(
    stream: UnixStream,
    registry: Arc<ClientRegistry>,
    events_tx: mpsc::UnboundedSender<RemoteEvent>,
) {
    let (read_half, mut write_half) = stream.into_split();
    let mut lines = BufReader::new(read_half).lines();

    // The handshake must come first; anything else is a protocol error.
    let info = match lines.next_line().await {
        Ok(Some(line)) => match decode::<ClientMsg>(&line) {
            Ok(ClientMsg::Attach {
                cols,
                rows,
                cwd,
                projects,
                terminal_colors,
            }) => AttachInfo {
                cols,
                rows,
                cwd,
                projects,
                terminal_colors,
            },
            Ok(ClientMsg::Kill) => {
                let _ = events_tx.send(RemoteEvent::Kill);
                return;
            }
            _ => return,
        },
        _ => return,
    };

    let (msg_tx, mut msg_rx) = mpsc::unbounded_channel::<ServerMsg>();
    let id = registry.register(info.cols, info.rows, info.cwd.clone(), msg_tx);
    let _cleanup = RemoveOnDrop {
        registry: registry.clone(),
        events_tx: events_tx.clone(),
        id,
    };
    if events_tx.send(RemoteEvent::Attached { id, info }).is_err() {
        return;
    }

    // Writer task: drains this client's queue into its socket half.
    let writer = tokio::spawn(async move {
        while let Some(msg) = msg_rx.recv().await {
            if write_half.write_all(encode(&msg).as_bytes()).await.is_err() {
                break;
            }
        }
        let _ = write_half.shutdown().await;
    });

    loop {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) | Err(_) => break, // EOF or read error: detach
        };
        let msg = match decode::<ClientMsg>(&line) {
            Ok(msg) => msg,
            Err(_) => continue, // tolerate junk lines (socat probing etc.)
        };
        let event = match msg {
            ClientMsg::Attach { .. } => continue, // duplicate handshake: ignore
            ClientMsg::Input(event) => RemoteEvent::Input { id, event },
            ClientMsg::Resize { cols, rows } => {
                registry.set_size(id, cols, rows);
                RemoteEvent::Resized
            }
            ClientMsg::EditResult(result) => RemoteEvent::EditResult { id, result },
            ClientMsg::ClipboardText(text) => RemoteEvent::ClipboardText { id, text },
            ClientMsg::Kill => RemoteEvent::Kill,
            ClientMsg::Detach => break,
        };
        if events_tx.send(event).is_err() {
            break;
        }
    }

    writer.abort();
    // `_cleanup` drops here: registry removal + Detached notification.
}
