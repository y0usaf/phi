//! Daemon lifecycle: socket/lock paths, the flock-based single-daemon
//! guarantee, stale-socket reaping, and detached spawn. Patterns follow
//! jcode's socket.rs: a live daemon holds the lock for its whole life, so
//! "socket refuses connections AND the lock is acquirable" proves the
//! socket is stale and safe to unlink.

use std::fs::OpenOptions;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};

/// How long the spawning client waits for the socket to answer before
/// giving up.
const READY_TIMEOUT: Duration = Duration::from_secs(5);

/// Runtime directory for the socket and lock. `$XDG_RUNTIME_DIR/phi` on
/// Linux; macOS has no runtime dir, so fall back to the data dir (same
/// location the panic log and auth store already use).
pub fn runtime_dir() -> PathBuf {
    let dirs = directories::ProjectDirs::from("", "", "phi");
    dirs.as_ref()
        .and_then(|d| d.runtime_dir().map(Path::to_path_buf))
        .or_else(|| dirs.as_ref().map(|d| d.data_dir().to_path_buf()))
        .unwrap_or_else(std::env::temp_dir)
}

pub fn socket_path() -> PathBuf {
    runtime_dir().join("mux.sock")
}

fn lock_path() -> PathBuf {
    runtime_dir().join("mux.lock")
}

pub fn log_path() -> PathBuf {
    directories::ProjectDirs::from("", "", "phi")
        .map(|d| d.data_dir().join("mux-daemon.log"))
        .unwrap_or_else(|| std::env::temp_dir().join("phi-mux-daemon.log"))
}

/// Held by the daemon for its entire life. Dropping the file releases the
/// flock, so a dead daemon (any exit path, including SIGKILL) provably
/// frees the lock.
pub struct DaemonLock {
    _file: std::fs::File,
}

/// Try to take the exclusive daemon lock without blocking. `Ok(None)`
/// means another live process holds it.
pub fn try_acquire_daemon_lock() -> Result<Option<DaemonLock>> {
    try_acquire_lock_at(&lock_path())
}

fn try_acquire_lock_at(path: &Path) -> Result<Option<DaemonLock>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .with_context(|| format!("open daemon lock {}", path.display()))?;
    let ret = unsafe {
        use std::os::fd::AsRawFd as _;
        libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB)
    };
    if ret == 0 {
        Ok(Some(DaemonLock { _file: file }))
    } else {
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::WouldBlock {
            Ok(None)
        } else {
            Err(err).context("flock daemon lock")
        }
    }
}

/// Whether a daemon is accepting connections on the socket right now.
fn socket_is_live(path: &Path) -> bool {
    UnixStream::connect(path).is_ok()
}

/// Ensure a daemon is running and its socket answers; spawn one if needed.
/// Called by every `phi --mux` invocation before attaching.
pub fn ensure_running() -> Result<()> {
    let socket = socket_path();
    if socket_is_live(&socket) {
        return Ok(());
    }
    if socket.exists() {
        // The socket file exists but refuses connections. Only unlink it if
        // we can also take the daemon lock — a live daemon always holds the
        // lock, so lock-acquired + dead-socket proves staleness.
        match try_acquire_daemon_lock()? {
            Some(lock) => {
                let _ = std::fs::remove_file(&socket);
                drop(lock); // release before the spawned daemon takes it
            }
            None => {
                // A daemon holds the lock but isn't answering (yet): most
                // likely mid-startup. Fall through to the wait loop.
                wait_for_socket(&socket, READY_TIMEOUT)?;
                return Ok(());
            }
        }
    }
    spawn_detached()?;
    wait_for_socket(&socket, READY_TIMEOUT)
}

/// Spawn `current_exe() --mux-server` fully detached (own session via
/// setsid, no tty). The caller's `wait_for_socket` connect-poll covers
/// readiness.
fn spawn_detached() -> Result<()> {
    use std::os::unix::process::CommandExt as _;
    use std::process::{Command, Stdio};

    let exe = std::env::current_exe().context("locate phi binary")?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path())
        .ok();

    let mut cmd = Command::new(exe);
    cmd.arg("--mux-server")
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    match log {
        Some(file) => {
            cmd.stderr(file);
        }
        None => {
            cmd.stderr(Stdio::null());
        }
    }
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn().context("spawn mux daemon")?;
    Ok(())
}

/// Poll-connect until the socket answers or the deadline passes.
fn wait_for_socket(path: &Path, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if socket_is_live(path) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "mux daemon did not come up within {}s (log: {})",
                timeout.as_secs(),
                log_path().display()
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Connect to a live daemon and ask it to shut down. Used by
/// `phi --mux --kill`. Returns false when no daemon is running.
pub fn kill_daemon() -> Result<bool> {
    use std::io::Write as _;
    let socket = socket_path();
    let Ok(mut stream) = UnixStream::connect(&socket) else {
        return Ok(false);
    };
    stream.write_all(super::protocol::encode(&super::protocol::ClientMsg::Kill).as_bytes())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_is_exclusive_and_released_on_drop() {
        // A tempdir lock path: exercises the flock semantics without
        // touching the real runtime dir, which is unwritable in the Nix
        // build sandbox (and may be held by a live daemon on a dev machine).
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("mux.lock");
        let first = try_acquire_lock_at(&path)
            .unwrap()
            .expect("first acquire must succeed on a fresh path");
        assert!(
            try_acquire_lock_at(&path).unwrap().is_none(),
            "second acquire must fail while the first guard is held"
        );
        drop(first);
        assert!(try_acquire_lock_at(&path).unwrap().is_some());
    }
}
