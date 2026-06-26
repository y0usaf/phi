//! Unix-domain-socket transport for the phi server/client protocol.
//!
//! Mirrors `ref/jcode`'s `transport` module: newline-delimited JSON over a
//! Unix socket. The server binds a `Listener`; each accepted connection yields a
//! `Stream` that is split into a `ReadHalf` and `WriteHalf` for async framing.

use anyhow::Result;
use std::path::PathBuf;

#[cfg(unix)]
pub use tokio::net::UnixListener as Listener;
#[cfg(unix)]
pub use tokio::net::UnixStream as Stream;
#[cfg(unix)]
pub use tokio::net::unix::OwnedReadHalf as ReadHalf;
#[cfg(unix)]
pub use tokio::net::unix::OwnedWriteHalf as WriteHalf;

/// Synchronous stream used for blocking probes (e.g. "is a live listener
/// answering on this socket?").
#[cfg(unix)]
pub use std::os::unix::net::UnixStream as SyncStream;

/// Default socket path: `$XDG_RUNTIME_DIR/phi.sock` or `$TMPDIR/phi.sock`.
pub fn socket_path() -> PathBuf {
    if let Ok(custom) = std::env::var("PHI_SOCKET") {
        return PathBuf::from(custom);
    }
    if let Ok(rt) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(rt).join("phi.sock");
    }
    if let Ok(tmp) = std::env::var("TMPDIR") {
        return PathBuf::from(tmp).join("phi.sock");
    }
    PathBuf::from("/tmp/phi.sock")
}

/// Debug socket path, derived from the main socket path.
pub fn debug_socket_path() -> PathBuf {
    let main = socket_path();
    let filename = main
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("phi.sock");
    let debug_filename = filename.replace(".sock", "-debug.sock");
    main.with_file_name(debug_filename)
}

/// Returns true if a Unix socket file exists at `path`.
///
/// Uses `metadata` + `is_socket` so a regular file at the path does not
/// pass the probe.
pub fn is_socket_path(path: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        std::fs::metadata(path).map_or(false, |m| m.file_type().is_socket())
    }
    #[cfg(not(unix))]
    {
        path.exists()
    }
}

/// Remove a socket file if present.
pub fn remove_socket(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
}

/// Create a connected pair of Unix streams (for in-process bridging/tests).
#[cfg(unix)]
pub fn stream_pair() -> std::io::Result<(Stream, Stream)> {
    Stream::pair()
}

/// Connect to a socket path.
///
/// Does not unlink the path on connection-refused: a client-side cleanup can
/// strand a live daemon behind an unlinked Unix socket pathname.
#[cfg(unix)]
pub async fn connect_socket(path: &std::path::Path) -> Result<Stream> {
    match Stream::connect(path).await {
        Ok(stream) => Ok(stream),
        Err(err) if err.kind() == std::io::ErrorKind::ConnectionRefused && path.exists() => {
            anyhow::bail!(
                "Socket exists but refused the connection at {}. Retry, or remove it after confirming no phi server is running.",
                path.display()
            )
        }
        Err(err) => Err(err.into()),
    }
}

/// Probe whether a live listener is answering on `path`.
#[cfg(unix)]
pub async fn socket_has_live_listener(path: &std::path::Path) -> bool {
    is_socket_path(path) && Stream::connect(path).await.is_ok()
}

/// Reap a provably-stale socket left behind by a dead daemon.
///
/// Safety: only unlink if no live listener answers.
#[cfg(unix)]
pub async fn reap_stale_socket_if_dead(path: &std::path::Path) -> bool {
    if !is_socket_path(path) {
        return false;
    }
    if socket_has_live_listener(path).await {
        return false;
    }
    remove_socket(path);
    if let Some(sibling) = sibling_socket_path(path) {
        remove_socket(&sibling);
    }
    true
}

/// Compute the sibling (main <-> debug) socket path.
fn sibling_socket_path(path: &std::path::Path) -> Option<PathBuf> {
    let filename = path.file_name()?.to_str()?;
    if let Some(base) = filename.strip_suffix("-debug.sock") {
        return Some(path.with_file_name(format!("{}.sock", base)));
    }
    if let Some(base) = filename.strip_suffix(".sock") {
        return Some(path.with_file_name(format!("{}-debug.sock", base)));
    }
    None
}

/// Remove a socket file and its sibling (main/debug) if present.
pub fn cleanup_socket_pair(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    if let Some(sibling) = sibling_socket_path(path) {
        let _ = std::fs::remove_file(sibling);
    }
}
