//! Daemon lifecycle: socket/lock paths, the flock-based single-daemon
//! guarantee, stale-socket reaping, detached spawn, and the ready-fd
//! handshake. Patterns follow jcode's socket.rs: a live daemon holds the
//! lock for its whole life, so "socket refuses connections AND the lock is
//! acquirable" proves the socket is stale and safe to unlink.

use std::fs::OpenOptions;
use std::io::Read as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};

/// Env var carrying the write end of the ready pipe to the spawned daemon.
pub const READY_FD_ENV: &str = "PHI_MUX_READY_FD";

/// How long the spawning client waits for the daemon's ready byte before
/// falling back to connect-polling.
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
    let path = lock_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
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
/// setsid, no tty), with a pipe handshake so this returns as soon as the
/// daemon's accept loop is live.
fn spawn_detached() -> Result<()> {
    use std::os::fd::FromRawFd as _;
    use std::os::unix::process::CommandExt as _;
    use std::process::{Command, Stdio};

    let exe = std::env::current_exe().context("locate phi binary")?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path())
        .ok();

    let mut fds = [0i32; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        bail!("pipe for daemon ready handshake failed");
    }
    let (read_fd, write_fd) = (fds[0], fds[1]);

    let mut cmd = Command::new(exe);
    cmd.arg("--mux-server")
        .env(READY_FD_ENV, write_fd.to_string())
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
    let spawned = cmd.spawn().context("spawn mux daemon");
    // Parent must close its copy of the write end, or the ready-read below
    // never sees EOF when the daemon dies before signaling.
    unsafe { libc::close(write_fd) };
    let mut ready = unsafe { std::fs::File::from_raw_fd(read_fd) };
    spawned?;

    // Wait for the single ready byte (or EOF on daemon death). A poll
    // timeout falls through to connect-polling in `wait_for_socket`.
    let ready_fd = {
        use std::os::fd::AsRawFd as _;
        ready.as_raw_fd()
    };
    let mut pfd = libc::pollfd {
        fd: ready_fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let timeout_ms = READY_TIMEOUT.as_millis() as i32;
    let n = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
    if n > 0 {
        let mut byte = [0u8; 1];
        let _ = ready.read(&mut byte);
    }
    // `ready` drops here, closing the read end; a poll timeout falls
    // through to connect-polling in `wait_for_socket`.
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

/// Daemon side: write the ready byte to the fd inherited via
/// [`READY_FD_ENV`] (if any) and close it. Call once, right after the
/// accept loop is listening.
pub fn signal_ready() {
    let Some(fd) = std::env::var(READY_FD_ENV)
        .ok()
        .and_then(|s| s.parse::<i32>().ok())
    else {
        return;
    };
    unsafe {
        let byte = [1u8; 1];
        libc::write(fd, byte.as_ptr().cast(), 1);
        libc::close(fd);
        // Don't leak the fd number into session subprocess environments.
        std::env::remove_var(READY_FD_ENV);
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
        // Uses the real lock path; fine for a single-user dev machine and
        // mirrors how the daemon actually behaves.
        let first = try_acquire_daemon_lock().unwrap();
        if first.is_none() {
            // A live daemon on this machine legitimately holds the lock;
            // nothing further to assert.
            return;
        }
        assert!(
            try_acquire_daemon_lock().unwrap().is_none(),
            "second acquire must fail while the first guard is held"
        );
        drop(first);
        assert!(try_acquire_daemon_lock().unwrap().is_some());
    }
}
