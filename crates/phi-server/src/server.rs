//! The phi server: a daemon that owns the RLM engine and provider, listens on
//! a Unix socket, and streams events to connected clients.
//!
//! Mirrors `ref/jcode`'s `server.rs` accept loop and per-connection handler,
//! scoped to phi's single-session RLM engine (no swarm/comm/ambient).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use anyhow::Result;
use phi_protocol::{
    Listener, Request, ServerEvent, Stream, debug_socket_path, remove_socket,
    socket_has_live_listener, socket_path,
    peek_request_id,
};
#[cfg(unix)]
use phi_protocol::{DaemonLock, daemon_lock_path_for, signal_ready};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

use crate::session::ServerSession;

/// Idle timeout for the shared server when no clients are connected (5 min).
const IDLE_TIMEOUT_SECS: u64 = 300;

/// Exit code when server shuts down due to idle timeout.
pub const EXIT_IDLE_TIMEOUT: i32 = 44;

/// Timeout for writing a single event to a client socket (5 seconds).
///
/// If a client's TCP/Unix buffer is full (frozen TUI, slow consumer) we
/// don't stall the forwarder indefinitely — we drop the connection after
/// this timeout so the RLM turn can abort cleanly.
const WRITE_TIMEOUT_SECS: u64 = 5;

/// The server state.
pub struct Server {
    socket_path: PathBuf,
    debug_socket_path: PathBuf,
    lock_path: PathBuf,
    /// Optional ready-pipe FD: when set, the server writes a single byte
    /// to this FD after sockets are bound and listeners are spawned.
    ready_fd: Option<std::os::fd::RawFd>,
    /// Active sessions (session_id -> ServerSession).
    sessions: Arc<RwLock<HashMap<String, Arc<Mutex<ServerSession>>>>>,
    /// Default session id.
    session_id: Arc<RwLock<String>>,
    /// Number of connected clients.
    client_count: Arc<RwLock<usize>>,
    /// Whether the server is currently processing a message.
    is_processing: Arc<RwLock<bool>>,
    /// Broadcast channel for shutdown.
    shutdown_tx: tokio::sync::broadcast::Sender<()>,
    /// Shared cancel token for the current generation. Cloned into the
    /// session before each `send()` so `cancel_current()` can fire it
    /// without holding the session mutex.
    current_cancel: Arc<RwLock<CancellationToken>>,
}

impl Server {
    /// Create a new server.
    pub fn new() -> Self {
        let (shutdown_tx, _) = tokio::sync::broadcast::channel(16);
        let socket_path = socket_path();
        let lock_path = daemon_lock_path_for(&socket_path);
        Self {
            socket_path,
            debug_socket_path: debug_socket_path(),
            lock_path,
            ready_fd: None,
            sessions: Arc::new(RwLock::new(HashMap::new())),
            session_id: Arc::new(RwLock::new(String::new())),
            client_count: Arc::new(RwLock::new(0)),
            is_processing: Arc::new(RwLock::new(false)),
            shutdown_tx,
            current_cancel: Arc::new(RwLock::new(CancellationToken::new())),
        }
    }

    /// Create a server with explicit socket paths (for testing).
    pub fn new_with_paths(socket_path: PathBuf, debug_socket_path: PathBuf) -> Self {
        let lock_path = daemon_lock_path_for(&socket_path);
        let (shutdown_tx, _) = tokio::sync::broadcast::channel(16);
        Self {
            socket_path,
            debug_socket_path,
            lock_path,
            ready_fd: None,
            sessions: Arc::new(RwLock::new(HashMap::new())),
            session_id: Arc::new(RwLock::new(String::new())),
            client_count: Arc::new(RwLock::new(0)),
            is_processing: Arc::new(RwLock::new(false)),
            shutdown_tx,
            current_cancel: Arc::new(RwLock::new(CancellationToken::new())),
        }
    }

    /// Set a ready-pipe file descriptor.
    ///
    /// When set, the server writes a single zero byte to this FD after
    /// sockets are bound and accept loops are spawned, eliminating startup
    /// race conditions for the parent process that spawned the daemon.
    #[cfg(unix)]
    pub fn set_ready_fd(&mut self, fd: std::os::fd::RawFd) {
        self.ready_fd = Some(fd);
    }

    /// Run the server: bind sockets, accept connections, dispatch requests.
    ///
    /// Blocks until a shutdown signal is received or the idle timeout fires.
    pub async fn run(&self) -> Result<()> {
        // Ensure socket directory exists.
        if let Some(parent) = self.socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Acquire the daemon singleton lock.  This prevents two daemons
        // from binding the same socket pair even if the socket probing
        // race condition fires.
        #[cfg(unix)]
        let _lock = {
            if let Some(parent) = self.lock_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            DaemonLock::acquire(&self.lock_path)?
        };

        if socket_has_live_listener(&self.socket_path).await {
            anyhow::bail!(
                "Refusing to replace active server socket at {}",
                self.socket_path.display()
            );
        }

        // Remove stale sockets.
        remove_socket(&self.socket_path);
        remove_socket(&self.debug_socket_path);

        let main_listener = Listener::bind(&self.socket_path)?;
        let debug_listener = Listener::bind(&self.debug_socket_path)?;

        // Restrict socket files to owner-only.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(
                &self.socket_path,
                std::fs::Permissions::from_mode(0o600),
            );
            let _ = std::fs::set_permissions(
                &self.debug_socket_path,
                std::fs::Permissions::from_mode(0o600),
            );
        }

        eprintln!(
            "phi server listening on {:?}",
            self.socket_path
        );

        let runtime = ServerRuntime {
            sessions: Arc::clone(&self.sessions),
            session_id: Arc::clone(&self.session_id),
            client_count: Arc::clone(&self.client_count),
            is_processing: Arc::clone(&self.is_processing),
            shutdown_tx: self.shutdown_tx.clone(),
            current_cancel: Arc::clone(&self.current_cancel),
        };

        let main_handle = runtime.clone().spawn_main_accept_loop(main_listener);
        let debug_handle = runtime.clone().spawn_debug_accept_loop(debug_listener);

        // Signal readiness to the parent process if a ready-pipe FD was set.
        #[cfg(unix)]
        if let Some(fd) = self.ready_fd {
            signal_ready(fd);
        }

        // Idle timeout monitor: shut down after no clients have been
        // connected for IDLE_TIMEOUT_SECS. Checks every 30 s so a brief
        // connect/disconnect doesn't trigger a false positive.
        let idle_client_count = Arc::clone(&self.client_count);
        let idle_shutdown_tx = self.shutdown_tx.clone();
        let _idle_handle = tokio::spawn(async move {
            let check_interval = std::time::Duration::from_secs(30);
            let mut idle_since: Option<std::time::Instant> = None;
            loop {
                tokio::time::sleep(check_interval).await;
                let count = *idle_client_count.read().await;
                if count == 0 {
                    match idle_since {
                        None => idle_since = Some(std::time::Instant::now()),
                        Some(t) => {
                            if t.elapsed().as_secs() >= IDLE_TIMEOUT_SECS {
                                eprintln!("phi server: idle timeout, shutting down");
                                let _ = idle_shutdown_tx.send(());
                                return;
                            }
                        }
                    }
                } else {
                    idle_since = None;
                }
            }
        });

        // Wait for shutdown signal.
        let mut shutdown_rx = self.shutdown_tx.subscribe();
        tokio::select! {
            _ = shutdown_rx.recv() => {}
            _ = main_handle => {}
            _ = debug_handle => {}
        }

        // Clean up: remove socket files and the lock file.
        remove_socket(&self.socket_path);
        remove_socket(&self.debug_socket_path);
        let _ = std::fs::remove_file(&self.lock_path);

        Ok(())
    }

    /// Check whether a server is reachable on the default socket.
    pub async fn is_running() -> bool {
        let socket = socket_path();
        socket_has_live_listener(&socket).await
    }

    /// Signal the server to shut down.
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(());
    }
}

/// Clonable handle to the server's shared state, passed to accept loops.
#[derive(Clone)]
struct ServerRuntime {
    sessions: Arc<RwLock<HashMap<String, Arc<Mutex<ServerSession>>>>>,
    session_id: Arc<RwLock<String>>,
    client_count: Arc<RwLock<usize>>,
    is_processing: Arc<RwLock<bool>>,
    shutdown_tx: tokio::sync::broadcast::Sender<()>,
    current_cancel: Arc<RwLock<CancellationToken>>,
}

impl ServerRuntime {
    fn spawn_main_accept_loop(self, listener: Listener) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        *self.client_count.write().await += 1;
                        let runtime = self.clone();
                        tokio::spawn(async move {
                            runtime.handle_client(stream).await;
                        });
                    }
                    Err(e) => {
                        eprintln!("Main accept error: {e}");
                    }
                }
            }
        })
    }

    fn spawn_debug_accept_loop(self, listener: Listener) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        let runtime = self.clone();
                        tokio::spawn(async move {
                            runtime.handle_debug_client(stream).await;
                        });
                    }
                    Err(e) => {
                        eprintln!("Debug accept error: {e}");
                    }
                }
            }
        })
    }

    /// Per-connection handler: read requests, dispatch, stream events back.
    async fn handle_client(self, stream: Stream) {
        let (reader, writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let writer = Arc::new(Mutex::new(writer));
        let mut line = String::new();

        loop {
            line.clear();
            let n = match reader.read_line(&mut line).await {
                Ok(n) => n,
                Err(e) => {
                    eprintln!("Client read error: {e}");
                    break;
                }
            };
            if n == 0 {
                break;
            }
            if line.trim().is_empty() {
                continue;
            }

            let request = match phi_protocol::decode_request(&line) {
                Ok(r) => r,
                Err(e) => {
                    // Peek the request id from the raw JSON so the client
                    // can correlate the error even if the variant was
                    // unknown or a field was missing.
                    let id = peek_request_id(&line);
                    write_event(
                        &writer,
                        &ServerEvent::Error {
                            id,
                            message: format!("Invalid request: {e}"),
                            retry_after_secs: None,
                        },
                    )
                    .await;
                    continue;
                }
            };

            self.clone().dispatch_request(request, &writer).await;
        }

        // Decrement client count on disconnect.
        {
            let mut count = self.client_count.write().await;
            if *count > 0 {
                *count -= 1;
            }
        }
    }

    /// Debug client handler: minimal — just echoes state.
    async fn handle_debug_client(self, stream: Stream) {
        let (reader, writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let writer = Arc::new(Mutex::new(writer));
        let mut line = String::new();

        loop {
            line.clear();
            let n = match reader.read_line(&mut line).await {
                Ok(n) => n,
                Err(_) => break,
            };
            if n == 0 {
                break;
            }
            if line.trim().is_empty() {
                continue;
            }
            let request = match phi_protocol::decode_request(&line) {
                Ok(r) => r,
                Err(_) => continue,
            };
            self.clone().dispatch_request(request, &writer).await;
        }
    }

    /// Dispatch a single request: send an ack, then perform the action and
    /// stream resulting events.
    async fn dispatch_request(self, request: Request, writer: &Arc<Mutex<tokio::net::unix::OwnedWriteHalf>>) {
        use Request::*;
        match request {
            Message { id, content } => {
                // Reject if another turn is already in progress.  Without
                // this guard, two concurrent clients can both pass through
                // and clobber each other's turn.
                if *self.is_processing.read().await {
                    write_event(writer, &ServerEvent::Error {
                        id,
                        message: "A turn is already in progress".to_string(),
                        retry_after_secs: None,
                    }).await;
                    return;
                }
                write_event(writer, &ServerEvent::Ack { id }).await;
                self.handle_message(id, content, writer).await;
            }
            Cancel { id } => {
                write_event(writer, &ServerEvent::Ack { id }).await;
                self.cancel_current().await;
            }
            Clear { id } => {
                write_event(writer, &ServerEvent::Ack { id }).await;
                self.clear_history().await;
            }
            Ping { id } => {
                write_event(writer, &ServerEvent::Pong { id }).await;
            }
            GetState { id: _ } => {
                let is_processing = *self.is_processing.read().await;
                let session_id = self.session_id.read().await.clone();
                let client_count = *self.client_count.read().await;
                write_event(
                    writer,
                    &ServerEvent::State {
                        is_processing,
                        session_id,
                        client_count,
                    },
                )
                .await;
            }
            Subscribe { id, working_dir, target_session_id } => {
                write_event(writer, &ServerEvent::Ack { id }).await;
                // Ensure a session exists for this client.
                self.ensure_session(working_dir, target_session_id).await;
            }
            GetHistory { id } => {
                write_event(writer, &ServerEvent::Ack { id }).await;
                let messages = self.get_history().await;
                write_event(writer, &ServerEvent::History { messages }).await;
            }
            ResumeSession { id, session_id } => {
                write_event(writer, &ServerEvent::Ack { id }).await;
                // Ensure the target session exists (load from disk if
                // needed) and switch the active session id to it.
                self.ensure_session(None, Some(session_id.clone())).await;
                *self.session_id.write().await = session_id;
            }
            Shutdown { id } => {
                write_event(writer, &ServerEvent::Ack { id }).await;
                let _ = self.shutdown_tx.send(());
            }
        }
    }

    /// Ensure a session exists, optionally rooted at `working_dir` or
    /// resuming a specific `target` session id.
    ///
    /// If `target` is provided:
    /// - If the session is already in the map, do nothing (it's loaded).
    /// - If not in the map but a state file exists on disk, build a session
    ///   and insert it.
    /// - If neither, emit a warning (the session id is still set by the
    ///   caller, but no session object is created).
    ///
    /// If `target` is `None`:
    /// - If a default session already exists, do nothing.
    /// - Otherwise build a fresh session from `working_dir` (or CWD).
    async fn ensure_session(&self, working_dir: Option<String>, target: Option<String>) {
        // If a target session id was specified, check if it already exists.
        if let Some(tid) = &target {
            if self.sessions.read().await.contains_key(tid) {
                return; // already loaded
            }
            // Try to build a session — this will pick up the session id
            // from the on-disk SessionStore if it exists.
            let cwd = working_dir
                .map(PathBuf::from)
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
            match crate::session::build_server_session(&cwd).await {
                Ok((id, session)) => {
                    // Only insert if the built session's id matches the target.
                    // If it doesn't match, the target session doesn't exist on
                    // disk — insert under the target id anyway so the caller's
                    // session_id assignment doesn't dangle.
                    let insert_id = if id == *tid { id } else { tid.clone() };
                    self.sessions
                        .write()
                        .await
                        .insert(insert_id, Arc::new(Mutex::new(session)));
                }
                Err(e) => {
                    eprintln!("Failed to build server session for resume: {e:?}");
                }
            }
            return;
        }

        // No target: ensure the default session exists.
        let sid = self.session_id.read().await;
        if !sid.is_empty() {
            return; // session already initialized
        }
        drop(sid);

        let cwd = working_dir
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

        match crate::session::build_server_session(&cwd).await {
            Ok((id, session)) => {
                *self.session_id.write().await = id.clone();
                self.sessions.write().await.insert(id, Arc::new(Mutex::new(session)));
            }
            Err(e) => {
                eprintln!("Failed to build server session: {e:?}");
            }
        }
    }

    /// Handle a Message request: run the RLM and stream events.
    ///
    /// A concurrent forwarder task reads `RlmEvent`s from the session's
    /// event channel *while* the RLM turn is running, bridges them to
    /// `ServerEvent`s, and writes them to the client — so clients see
    /// text deltas and tool starts in real time, not a burst after the
    /// turn completes.
    async fn handle_message(
        &self,
        id: u64,
        content: String,
        writer: &Arc<Mutex<tokio::net::unix::OwnedWriteHalf>>,
    ) {
        // Ensure a session exists.
        self.ensure_session(None, None).await;

        let session = {
            let sessions = self.sessions.read().await;
            let sid = self.session_id.read().await;
            sessions.get(&*sid).cloned()
        };

        let Some(session) = session else {
            write_event(
                writer,
                &ServerEvent::Error {
                    id,
                    message: "No active session".to_string(),
                    retry_after_secs: None,
                },
            )
            .await;
            return;
        };

        *self.is_processing.write().await = true;

        // --- Concurrent event streaming ---
        //
        // 1. Lock the session, take events_rx out, unlock.
        // 2. Spawn a forwarder that reads events_rx.recv().await and
        //    bridges RlmEvents → ServerEvents → client writer.
        // 3. Lock the session again, call send() (runs the RLM turn).
        //    Events emitted during the turn are picked up by the forwarder.
        // 4. After send() completes, signal the forwarder to stop via a
        //    oneshot, drain any remaining events, return events_rx to the
        //    session.
        // 5. Write the terminal Done/Error event.

        let mut s = session.lock().await;
        // Wire the shared cancel token into the session so cancel_current()
        // can fire it without holding the session mutex. We clone the shared
        // token into the session; cancel_current() fires the shared one.
        let turn_cancel = self.current_cancel.read().await.clone();
        s.set_cancel(turn_cancel.clone());
        let events_rx = s.take_events_rx();

        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
        let writer_clone = Arc::clone(writer);

        let forwarder = tokio::spawn(async move {
            let mut done_rx = done_rx;
            // If there's no event receiver, just wait for the done signal.
            let mut events_rx = match events_rx {
                Some(rx) => rx,
                None => {
                    let _ = done_rx.await;
                    return None;
                }
            };
            let mut write_failed = false;
            loop {
                if write_failed {
                    // Client is gone — drain events without writing, then
                    // exit so the session lock is released promptly.
                    while events_rx.try_recv().is_ok() {}
                    break;
                }
                tokio::select! {
                    // A new RlmEvent arrived — bridge and write it.
                    event = events_rx.recv() => {
                        match event {
                            Some(ev) => {
                                if let Some(se) = crate::session::bridge_event(&ev) {
                                    if try_write_event(&writer_clone, &se).await.is_err() {
                                        // Client disconnected or write timed
                                        // out — fire the cancel token so the
                                        // upstream RLM generation stops
                                        // immediately, saving API tokens.
                                        turn_cancel.cancel();
                                        write_failed = true;
                                    }
                                }
                            }
                            None => break, // channel closed
                        }
                    }
                    // Turn finished — drain remaining events then exit.
                    _ = &mut done_rx => {
                        while let Ok(ev) = events_rx.try_recv() {
                            if !write_failed {
                                if let Some(se) = crate::session::bridge_event(&ev) {
                                    if try_write_event(&writer_clone, &se).await.is_err() {
                                        write_failed = true;
                                    }
                                }
                            }
                        }
                        break;
                    }
                }
            }
            // Return the receiver so the session can reuse it next turn.
            Some(events_rx)
        });

        // Run the RLM turn. Events stream to the client via the forwarder.
        let result = s.send(&content).await;

        // Signal the forwarder to drain and stop.
        let _ = done_tx.send(());
        // Wait for the forwarder to finish writing all events and get
        // the receiver back. Use `ok()` instead of `unwrap()` so a
        // forwarder panic doesn't crash the connection handler.
        let events_rx = match forwarder.await.ok().and_then(|r| r) {
            Some(rx) => rx,
            None => {
                // Forwarder panicked or had no receiver — nothing to return.
                *self.is_processing.write().await = false;
                *self.current_cancel.write().await = CancellationToken::new();
                write_event(writer, &ServerEvent::Error {
                    id,
                    message: "Internal error: event forwarder failed".to_string(),
                    retry_after_secs: None,
                }).await;
                return;
            }
        };
        s.return_events_rx(events_rx);

        *self.is_processing.write().await = false;

        // Reset the shared cancel token for the next turn.
        *self.current_cancel.write().await = CancellationToken::new();

        // Emit the terminal event.
        if let Some(err) = &result.error {
            write_event(
                writer,
                &ServerEvent::Error {
                    id,
                    message: err.clone(),
                    retry_after_secs: None,
                },
            )
            .await;
        } else {
            write_event(writer, &ServerEvent::Done { id }).await;
        }
    }

    /// Cancel the current generation.
    ///
    /// Fires the shared cancel token directly — no session mutex needed —
    /// so cancel works even while `handle_message` holds the lock.
    async fn cancel_current(&self) {
        let cc = self.current_cancel.read().await;
        cc.cancel();
    }

    /// Clear conversation history.
    async fn clear_history(&self) {
        let sessions = self.sessions.read().await;
        let sid = self.session_id.read().await;
        if let Some(session) = sessions.get(&*sid) {
            session.lock().await.clear().await;
        }
    }

    /// Get conversation history.
    async fn get_history(&self) -> Vec<phi_protocol::HistoryMessage> {
        let sessions = self.sessions.read().await;
        let sid = self.session_id.read().await;
        if let Some(session) = sessions.get(&*sid) {
            session.lock().await.history()
        } else {
            Vec::new()
        }
    }
}

/// Write a single ServerEvent as NDJSON to the (mutex-guarded) writer.
///
/// Returns `Err` if the write fails (broken pipe, timeout) so the caller
/// can abort the upstream generation and stop wasting API tokens.
async fn try_write_event(
    writer: &Arc<Mutex<tokio::net::unix::OwnedWriteHalf>>,
    event: &ServerEvent,
) -> std::result::Result<(), std::io::Error> {
    let json = match phi_protocol::encode_event(event) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("Event encode error: {e}");
            return Ok(()); // encoding bug, not a transport error
        }
    };
    let mut w = writer.lock().await;
    // Bounded write: if the client is frozen and its buffer is full, we
    // time out after WRITE_TIMEOUT_SECS rather than stalling the forwarder
    // indefinitely.
    let write_result = tokio::time::timeout(
        std::time::Duration::from_secs(WRITE_TIMEOUT_SECS),
        w.write_all(json.as_bytes()),
    )
    .await;
    match write_result {
        Ok(inner) => inner,
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "client write timeout",
        )),
    }
}

/// Best-effort write: ignores transport errors.  Used for acks, state
/// responses, and terminal events where there is nothing more to do if
/// the client has already disconnected.
async fn write_event(
    writer: &Arc<Mutex<tokio::net::unix::OwnedWriteHalf>>,
    event: &ServerEvent,
) {
    let _ = try_write_event(writer, event).await;
}
