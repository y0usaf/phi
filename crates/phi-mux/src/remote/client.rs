//! Thin terminal client: owns the real tty (raw mode, mouse capture,
//! color probe), forwards parsed input events to the daemon, and writes
//! the daemon's pre-rendered ANSI frames verbatim to stdout.

use std::io::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, Result};
use futures::StreamExt as _;
use phi_tui::RawModeGuard;
use phi_tui::crossterm::event::{DisableMouseCapture, EnableMouseCapture, Event, EventStream};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;

use super::daemon;
use super::protocol::{ClientMsg, ServerMsg, decode, encode};
use crate::clipboard::MuxClipboard;
use phi_session_tui::tui;

/// Attach to the mux daemon and mirror it on this terminal until the
/// daemon exits, the user detaches, or the connection drops.
pub async fn run(cwd: PathBuf, projects: Vec<String>, model: Option<String>) -> Result<()> {
    // The client owns the tty now, so it owns crash recovery too: restore
    // cooked mode on panics and fatal signals.
    std::panic::set_hook(Box::new(|info| {
        phi_tui::emergency_restore();
        eprintln!("phi mux client panicked: {info}");
    }));

    let guard = RawModeGuard::new().context("mux requires a tty")?;
    tui::restore_terminal_on_signals();
    let mut stdout = std::io::stdout();
    let _ = phi_tui::crossterm::execute!(stdout, EnableMouseCapture);
    let _ = write!(stdout, "\x1b[2J\x1b[H");
    let _ = stdout.flush();

    // Probe this terminal's colors inside raw mode; the daemon derives its
    // palette from the FIRST client's report (later attaches don't re-theme).
    let terminal_colors = phi_tui::detect_terminal_colors(Duration::from_millis(150));
    let (cols, rows) = phi_tui::crossterm::terminal::size().unwrap_or((80, 24));

    let stream = UnixStream::connect(daemon::socket_path())
        .await
        .context("connect to mux daemon")?;
    let (read_half, mut write_half) = stream.into_split();
    let mut lines = BufReader::new(read_half).lines();

    send(
        &mut write_half,
        &ClientMsg::Attach {
            cols,
            rows,
            cwd,
            projects,
            terminal_colors,
            model,
            // WS8.3: report this terminal's shell so daemon sessions
            // describe the attaching client's environment, not the first
            // spawner's.
            shell: std::env::var("SHELL")
                .or_else(|_| std::env::var("COMSPEC"))
                .ok(),
        },
    )
    .await?;

    let mut events = EventStream::new();
    let mut clipboard = MuxClipboard::default();

    let exit_reason: Option<String> = loop {
        tokio::select! {
            maybe = events.next() => {
                let msg = match maybe {
                    Some(Ok(Event::Resize(cols, rows))) => ClientMsg::Resize { cols, rows },
                    Some(Ok(event)) => ClientMsg::Input(event),
                    Some(Err(_)) | None => break None, // stdin gone: detach
                };
                if send(&mut write_half, &msg).await.is_err() {
                    break Some("mux daemon disconnected".to_owned());
                }
            }
            line = lines.next_line() => {
                let line = match line {
                    Ok(Some(line)) => line,
                    Ok(None) => break Some("mux daemon disconnected".to_owned()),
                    Err(err) => break Some(format!("mux connection error: {err}")),
                };
                let Ok(msg) = decode::<ServerMsg>(&line) else { continue };
                match msg {
                    ServerMsg::Frame(frame) => {
                        let _ = stdout.write_all(frame.as_bytes());
                        let _ = stdout.flush();
                    }
                    ServerMsg::EditRequest { text } => {
                        // Run $EDITOR on THIS terminal: leave raw mode, hand
                        // the tty over, then clear — the daemon follows up
                        // with a full repaint once it sees the result.
                        let _ = phi_tui::crossterm::execute!(stdout, DisableMouseCapture);
                        let result = guard.with_suspended(|| tui::external_editor(&text));
                        let _ = phi_tui::crossterm::execute!(stdout, EnableMouseCapture);
                        let _ = write!(stdout, "\x1b[2J\x1b[H");
                        let _ = stdout.flush();
                        if send(&mut write_half, &ClientMsg::EditResult(result)).await.is_err() {
                            break Some("mux daemon disconnected".to_owned());
                        }
                    }
                    ServerMsg::ClipboardRequest => {
                        let text = clipboard.read_text();
                        if send(&mut write_half, &ClientMsg::ClipboardText(text)).await.is_err() {
                            break Some("mux daemon disconnected".to_owned());
                        }
                    }
                    ServerMsg::Exit { reason } => break Some(reason),
                }
            }
        }
    };

    // Socket close alone detaches us server-side.
    let _ = write_half.shutdown().await;

    let _ = phi_tui::crossterm::execute!(stdout, DisableMouseCapture);
    let _ = write!(stdout, "\x1b[2J\x1b[H\x1b[?25h");
    let _ = stdout.flush();
    drop(guard);
    if let Some(reason) = exit_reason {
        eprintln!("phi mux: {reason}");
    }
    Ok(())
}

async fn send(write_half: &mut OwnedWriteHalf, msg: &ClientMsg) -> std::io::Result<()> {
    write_half.write_all(encode(msg).as_bytes()).await
}
