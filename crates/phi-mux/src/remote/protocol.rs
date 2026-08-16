//! Wire protocol between the mux daemon and its thin terminal clients.
//!
//! Framing is newline-delimited JSON: one message per line, in both
//! directions. Frame payloads are `String` (phi-grid emits valid UTF-8 ANSI
//! text; JSON string-escaping keeps embedded escapes/newlines line-safe),
//! so the whole protocol stays debuggable with `socat`.

use std::path::PathBuf;

use phi_tui::TerminalColors;
use phi_tui::crossterm::event::Event;
use serde::{Deserialize, Serialize};

/// Client → server. The first message on every connection must be `Attach`.
#[derive(Debug, Serialize, Deserialize)]
pub enum ClientMsg {
    /// Handshake: the client's terminal size, working directory, extra
    /// project dirs, and (when the OSC probe succeeded) its terminal colors.
    Attach {
        cols: u16,
        rows: u16,
        cwd: PathBuf,
        projects: Vec<String>,
        terminal_colors: Option<TerminalColors>,
        /// WS8.3: per-invocation `--model` override reaching a running
        /// daemon. `None` keeps the daemon's configured model; `Some` is
        /// applied to new sessions spawned for this client's projects.
        model: Option<String>,
        /// WS8.3: the client's `$SHELL` (or `$COMSPEC` on Windows) so the
        /// daemon's sessions report the attaching client's shell in the
        /// `# Environment` prompt block instead of the first spawner's.
        /// `None` leaves the daemon's own env as the fallback.
        shell: Option<String>,
    },
    /// A parsed terminal event (key/mouse/paste/focus), serialized verbatim.
    /// The client owns the tty and its quirks (kitty protocol etc.); the
    /// daemon consumes these exactly where local events were consumed.
    Input(Event),
    Resize {
        cols: u16,
        rows: u16,
    },
    /// Reply to [`ServerMsg::EditRequest`].
    EditResult(Result<Option<String>, String>),
    /// Reply to [`ServerMsg::ClipboardRequest`].
    ClipboardText(Option<String>),
    /// Terminate the daemon (sent by `phi --mux --kill`).
    Kill,
}

/// Server → client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMsg {
    /// Pre-rendered ANSI (already wrapped in synchronized-update markers by
    /// the renderer). Write verbatim to stdout.
    Frame(String),
    /// Run `$VISUAL`/`$EDITOR` on the client's tty over `text`; reply with
    /// `EditResult`.
    EditRequest { text: String },
    /// Read the client's local system clipboard; reply with `ClipboardText`.
    ClipboardRequest,
    /// The daemon is going away (quit/kill/shutdown); restore and exit.
    Exit { reason: String },
}

/// Encode a message as one JSON line (including the trailing newline).
pub fn encode<T: Serialize>(msg: &T) -> String {
    let mut line = serde_json::to_string(msg).expect("protocol messages always serialize");
    line.push('\n');
    line
}

/// Decode one JSON line into a message.
pub fn decode<'a, T: Deserialize<'a>>(line: &'a str) -> Result<T, serde_json::Error> {
    serde_json::from_str(line.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;
    use phi_tui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn round_trips_input_events() {
        let msg = ClientMsg::Input(Event::Key(KeyEvent::new(
            KeyCode::Char('q'),
            KeyModifiers::CONTROL,
        )));
        let line = encode(&msg);
        assert!(line.ends_with('\n'));
        let back: ClientMsg = decode(&line).unwrap();
        match back {
            ClientMsg::Input(Event::Key(key)) => {
                assert_eq!(key.code, KeyCode::Char('q'));
                assert_eq!(key.modifiers, KeyModifiers::CONTROL);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn frames_survive_embedded_escapes_and_newlines() {
        let ansi = "\x1b[?2026h\x1b[2J\x1b[Hhello\nworld\x1b[?2026l".to_owned();
        let line = encode(&ServerMsg::Frame(ansi.clone()));
        // The frame's own newline must be escaped, not break the framing.
        assert_eq!(line.matches('\n').count(), 1);
        let back: ServerMsg = decode(&line).unwrap();
        match back {
            ServerMsg::Frame(s) => assert_eq!(s, ansi),
            other => panic!("unexpected: {other:?}"),
        }
    }
}
