//! driving a persistent `RlmSession` as a coding agent. Auth is handled
//! in-TUI via /login, mirroring ref/phi: the OAuth flow runs in the
//! background, the URL lands in scrollback, and the editor doubles as the
//! paste-the-redirect-URL input (racing the localhost callback server).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use phi_ai::{
    AuthStorage, LoginCallbacks, LoginMethod, Model, OAuthProviderId, Registry, ThinkingLevel,
    Usage,
};
use phi_rlm::{
    ContextPayload, ProviderLm, ReplResult, RestoredExchange, RestoredRepl, Rlm, RlmCompletion,
    RlmEvent, StopCause, could_open_block, find_code_blocks, split_at_first_block, split_prose,
};
use phi_tui::crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use phi_tui::{
    Editor, EditorAction, LiveCursor, RawModeGuard, Selector, SelectorItem, SelectorOutcome,
    Spinner, Terminal, fill_bg, fuzzy_filter, render_markdown, wrap_ansi,
};

use crate::files::{self, FileSearcher};
use crate::mux::ui::overlays::usage::usage_overlay_lines;
use crate::mux::usage::load_usage_report;
use crate::session::{self, SessionStore};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const CYAN: &str = "\x1b[36m";
const YELLOW: &str = "\x1b[33m";

// pi's dark-theme container backgrounds (theme/dark.json). Repl blocks share
// the user-message gray so the green/red tool tints nested inside them stand
// out; only a failed exec recolors the repl container itself (red).
const USER_BG: &str = "\x1b[48;2;52;53;65m"; // userMessageBg #343541
const REPL_BG: &str = USER_BG;
const REPL_ERROR_BG: &str = "\x1b[48;2;70;40;45m"; // #46282d
const TOOL_SUCCESS_BG: &str = "\x1b[48;2;40;50;40m"; // toolSuccessBg #283228
const TOOL_ERROR_BG: &str = "\x1b[48;2;60;40;40m"; // toolErrorBg #3c2828

const CODE_PREVIEW_LINES: usize = 8;
const OUTPUT_PREVIEW_LINES: usize = 5;

/// Byte offset of the first position in `text` that is, or could still grow
/// into, a ```repl fence. The engine's block regex matches mid-line (the
/// model sometimes glues ```repl to the end of a sentence), so this scans
/// raw byte positions rather than line starts.
fn first_block_start(text: &str) -> usize {
    text.char_indices()
        .filter(|&(_, c)| c == '`')
        .map(|(i, _)| i)
        .find(|&i| could_open_block(&text[i..]))
        .unwrap_or(text.len())
}

/// Byte length of the prefix of `text` that is safe to flush into scrollback
/// while streaming: it ends at a completed blank line, leaves no code fence
/// open, and stops before anything that is or could become a ```repl block
/// (which `CodeExecStart` will consume from the stream).
fn stable_prefix(text: &str) -> usize {
    let limit = first_block_start(text);
    let mut fence_open = false;
    let mut boundary = 0;
    let mut pos = 0;
    for line in text.split_inclusive('\n') {
        pos += line.len();
        if pos > limit {
            break;
        }
        if line.trim_start().starts_with("```") {
            fence_open = !fence_open;
        } else if !fence_open && line.ends_with('\n') && line.trim().is_empty() {
            boundary = pos;
        }
    }
    boundary
}

/// Body of the first live ```repl block once its header newline has landed.
/// Partial headers still return `None`; a closing fence, if already present,
/// is excluded so the body can be previewed before `CodeExecStart` arrives.
fn live_block_body(text: &str) -> Option<&str> {
    let start = first_block_start(text);
    let tail = &text[start..];
    let rest = tail.strip_prefix("```repl")?;
    let mut header_len = "```repl".len();
    let mut saw_newline = false;
    for (i, c) in rest.char_indices() {
        match c {
            ' ' | '\t' => continue,
            '\n' => {
                header_len += i + 1;
                saw_newline = true;
                break;
            }
            _ => return None,
        }
    }
    if !saw_newline {
        return None;
    }
    let body = &tail[header_len..];
    Some(body.split_once("\n```").map_or(body, |(body, _)| body))
}

/// Gutter for the first line of an action at `depth`: one dim `│` column per
/// enclosing level, then a cyan `⏺` anchor.
fn dot_prefix(depth: usize) -> String {
    format!("{DIM}{}{RESET}{CYAN}⏺{RESET} ", "│ ".repeat(depth))
}

/// Gutter for continuation/result lines at `depth`: depth+1 dim `│` columns.
fn bar_prefix(depth: usize) -> String {
    format!("{DIM}{}{RESET}", "│ ".repeat(depth + 1))
}

/// Cap persisted tool results at roughly the replay preview budget. The full
/// output already lives in the repl stdout payload; storing it twice only
/// bloats the log with bytes nothing ever reads back.
const TOOL_RESULT_LOG_BUDGET: usize = 4096;

fn truncate_for_log(text: &str) -> String {
    if text.len() <= TOOL_RESULT_LOG_BUDGET {
        return text.to_owned();
    }
    let mut end = TOOL_RESULT_LOG_BUDGET;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[truncated {} bytes]", &text[..end], text.len() - end)
}

/// Title line for a nested tool block: the tool's defining argument, pi-style
/// (`$ cmd` for bash, `read path`, ...), falling back to `name key=value`.
fn tool_title(name: &str, args: &serde_json::Value) -> String {
    let arg = |key: &str| {
        args.get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
    };
    let one_line = |text: &str| {
        let flat = text.lines().next().unwrap_or("").trim().to_owned();
        let short: String = flat.chars().take(80).collect();
        if short.len() < flat.len() || text.lines().count() > 1 {
            format!("{short}…")
        } else {
            short
        }
    };
    let in_path = |path: &str| {
        if path.is_empty() || path == "." {
            String::new()
        } else {
            format!(" in {path}")
        }
    };
    match name {
        "bash" => format!("{BOLD}${RESET} {}", one_line(arg("cmd"))),
        "read_file" => format!("{BOLD}read{RESET} {}", arg("path")),
        "write_file" => format!("{BOLD}write{RESET} {}", arg("path")),
        "edit_file" => format!("{BOLD}edit{RESET} {}", arg("path")),
        "grep" => format!(
            "{BOLD}grep{RESET} {}{}",
            one_line(arg("pattern")),
            in_path(arg("path"))
        ),
        "find" => format!(
            "{BOLD}find{RESET} {}{}",
            one_line(arg("pattern")),
            in_path(arg("path"))
        ),
        "ls" => format!("{BOLD}ls{RESET} {}", arg("path"))
            .trim_end()
            .to_owned(),
        _ => match args.as_object() {
            Some(map) if !map.is_empty() => {
                let pairs: Vec<String> = map
                    .iter()
                    .map(|(key, value)| match value {
                        serde_json::Value::String(s) => format!("{key}={}", one_line(s)),
                        other => format!("{key}={other}"),
                    })
                    .collect();
                format!("{BOLD}{name}{RESET} {}", pairs.join(" "))
            }
            _ => format!("{BOLD}{name}{RESET}"),
        },
    }
}

fn format_tokens(count: u64) -> String {
    match count {
        0..=999 => count.to_string(),
        1_000..=9_999 => format!("{:.1}k", count as f64 / 1_000.0),
        10_000..=999_999 => format!("{}k", count / 1_000),
        _ => format!("{:.1}M", count as f64 / 1_000_000.0),
    }
}

enum Cmd {
    Prompt {
        text: String,
        cancel: CancellationToken,
    },
    /// `!command` / `!!command` shell escape, executed in the session REPL.
    Shell {
        code: String,
        /// Whether the model sees the execution on its next turn.
        record: bool,
    },
    Reset,
    /// Rebuild env history from persisted exchanges (resume/fork/tree).
    Restore {
        exchanges: Vec<RestoredExchange>,
        pending: Vec<RestoredRepl>,
    },
    Shutdown,
}

enum Done {
    Run(RlmCompletion),
    Shell {
        code: String,
        record: bool,
        result: Result<ReplResult, String>,
    },
    Restored(Result<(), String>),
}

/// Quote `s` as a Python string literal.
fn python_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

enum LoginMsg {
    Info(String),
    Done {
        provider: &'static str,
        result: Result<(), String>,
    },
}

pub struct TuiSetup {
    pub rlm: Rlm,
    pub payload: ContextPayload,
    pub events: mpsc::UnboundedReceiver<RlmEvent>,
    /// Providers that had no credentials at startup.
    pub missing_credentials: Vec<String>,
    pub ui: UiSetup,
}

pub struct UiSetup {
    pub max_iterations: usize,
    pub storage: Arc<AuthStorage>,
    pub registry: Arc<Registry>,
    /// Root model handle; /model swaps it in place.
    pub root_lm: Arc<ProviderLm>,
    /// Sub model handle (llm_query default); /submodel swaps it.
    pub sub_lm: Arc<ProviderLm>,
    /// On-disk session store recording the conversation tree.
    pub store: SessionStore,
}

pub struct SuspendRequest {
    pub text: String,
    pub response_tx: oneshot::Sender<std::result::Result<Option<String>, String>>,
}

#[derive(Clone, Debug)]
pub enum HostModalKind {
    Model { root: bool, current: String },
    Usage,
}

pub struct HostModalRequest {
    pub kind: HostModalKind,
    pub response_tx: oneshot::Sender<std::result::Result<Option<String>, String>>,
}

struct PendingHostModal {
    kind: HostModalKind,
    response_rx: oneshot::Receiver<std::result::Result<Option<String>, String>>,
}

pub struct SessionIo {
    pub writer: Option<Box<dyn std::io::Write + Send>>,
    pub event_rx: Option<mpsc::UnboundedReceiver<Event>>,
    pub dimensions: Option<(u16, u16)>,
    pub embedded: bool,
    pub suspend_tx: Option<mpsc::UnboundedSender<SuspendRequest>>,
    pub host_modal_tx: Option<mpsc::UnboundedSender<HostModalRequest>>,
    pub cwd: PathBuf,
}

impl SessionIo {
    pub fn standalone(cwd: PathBuf) -> Self {
        Self {
            writer: None,
            event_rx: None,
            dimensions: None,
            embedded: false,
            suspend_tx: None,
            host_modal_tx: None,
            cwd,
        }
    }
}

enum InputSource {
    Tty(EventStream),
    Forwarded(mpsc::UnboundedReceiver<Event>),
    Closed,
}

impl InputSource {
    async fn next(&mut self) -> Option<std::io::Result<Event>> {
        match self {
            Self::Tty(events) => {
                use futures::StreamExt as _;
                events.next().await
            }
            Self::Forwarded(rx) => rx.recv().await.map(Ok),
            Self::Closed => None,
        }
    }
}

struct CommandSpec {
    name: &'static str,
    args: &'static str,
    desc: &'static str,
}

const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: "login",
        args: "[provider]",
        desc: "authenticate via OAuth (anthropic | openai)",
    },
    CommandSpec {
        name: "logout",
        args: "[provider]",
        desc: "remove stored credentials",
    },
    CommandSpec {
        name: "model",
        args: "[model]",
        desc: "switch the root model (no arg: open the selector)",
    },
    CommandSpec {
        name: "submodel",
        args: "[model]",
        desc: "switch the sub model used by llm_query",
    },
    CommandSpec {
        name: "resume",
        args: "",
        desc: "pick a past session for this directory to resume",
    },
    CommandSpec {
        name: "tree",
        args: "",
        desc: "navigate the session tree (also esc esc)",
    },
    CommandSpec {
        name: "fork",
        args: "",
        desc: "fork a new session from a prior user message",
    },
    CommandSpec {
        name: "new",
        args: "",
        desc: "reset the session (fresh REPL, history cleared)",
    },
    CommandSpec {
        name: "usage",
        args: "",
        desc: "show usage totals",
    },
    CommandSpec {
        name: "help",
        args: "",
        desc: "show commands and keys",
    },
    CommandSpec {
        name: "quit",
        args: "",
        desc: "exit (also ctrl-d on an empty line)",
    },
];

#[derive(Clone, Copy, Eq, PartialEq)]
enum PopupKind {
    /// Completing a command name; enter applies and submits.
    Command,
    /// Completing a command argument; enter only applies.
    Argument,
    /// `@token` fuzzy file search; directories keep completing.
    File,
    /// Tab path completion (no `@` prefix).
    Path,
}

/// What a confirmed selector value means (pi-style editor-swap selectors).
enum SelectorKind {
    /// Value is a `provider/id` model name; true switches the root model.
    Model { root: bool },
    /// Read-only usage browser; confirm just closes it.
    Usage,
    /// Value is a session file path to resume.
    Resume,
    /// Value is the entry id of a user message to fork from.
    Fork,
    /// Value is the entry id to navigate the active branch to.
    Tree,
}

struct Suggestion {
    /// Text inserted on completion (command name without the slash, or the
    /// argument value).
    value: String,
    label: String,
    desc: String,
}

struct Popup {
    items: Vec<Suggestion>,
    selected: usize,
    /// The text ending at the cursor that a completion replaces.
    prefix: String,
    kind: PopupKind,
}

#[derive(Eq, PartialEq)]
enum PopupOutcome {
    /// The key was consumed by the popup.
    Handled,
    /// Let the regular input path process the key.
    Pass,
}

const POPUP_MAX_VISIBLE: usize = 6;

fn render_popup(popup: &Popup) -> Vec<String> {
    let total = popup.items.len();
    let start = popup
        .selected
        .saturating_sub(POPUP_MAX_VISIBLE - 1)
        .min(total.saturating_sub(POPUP_MAX_VISIBLE));
    let width = popup
        .items
        .iter()
        .map(|item| item.label.len())
        .max()
        .unwrap_or(0);
    let mut lines: Vec<String> = popup
        .items
        .iter()
        .enumerate()
        .skip(start)
        .take(POPUP_MAX_VISIBLE)
        .map(|(i, item)| {
            if i == popup.selected {
                format!(
                    "{CYAN}› {:<width$}{RESET}  {DIM}{}{RESET}",
                    item.label, item.desc
                )
            } else {
                format!("{DIM}  {:<width$}  {}{RESET}", item.label, item.desc)
            }
        })
        .collect();
    if total > start + POPUP_MAX_VISIBLE {
        lines.push(format!(
            "{DIM}  … {} more{RESET}",
            total - start - POPUP_MAX_VISIBLE
        ));
    }
    lines
}

/// Suggestions for the text before the cursor, pi-style: `/prefix` fuzzy
/// filters command names; `/command arg-prefix` filters that command's
/// arguments.
fn complete(before: &str, model_names: &[String]) -> Option<Popup> {
    let rest = before.strip_prefix('/')?;
    let Some((command, arg)) = rest.split_once(' ') else {
        let items: Vec<Suggestion> =
            fuzzy_filter(COMMANDS.iter().collect(), rest, |c| c.name.to_owned())
                .into_iter()
                .map(|c| Suggestion {
                    value: c.name.to_owned(),
                    label: format!("/{} {}", c.name, c.args).trim_end().to_owned(),
                    desc: c.desc.to_owned(),
                })
                .collect();
        return (!items.is_empty()).then(|| Popup {
            items,
            selected: 0,
            prefix: before.to_owned(),
            kind: PopupKind::Command,
        });
    };
    if arg.contains(' ') {
        return None;
    }
    let candidates: Vec<&str> = match command {
        "login" | "logout" => vec!["anthropic", "openai"],
        "model" | "submodel" => model_names.iter().map(String::as_str).collect(),
        _ => return None,
    };
    let items: Vec<Suggestion> = fuzzy_filter(candidates, arg, |c| (*c).to_owned())
        .into_iter()
        .map(|c| Suggestion {
            value: c.to_owned(),
            label: c.to_owned(),
            desc: String::new(),
        })
        .collect();
    (!items.is_empty()).then(|| Popup {
        items,
        selected: 0,
        prefix: arg.to_owned(),
        kind: PopupKind::Argument,
    })
}

/// The `@`-prefixed token ending at the cursor, if any. Tokens break at
/// spaces/tabs/quotes/`=`, except inside an unclosed `@"…` quote.
fn extract_at_token(before: &str) -> Option<String> {
    // Unclosed `@"…` quote: the token runs from the `@` to the cursor.
    if let Some(qstart) = find_unclosed_quote(before)
        && qstart > 0
        && before[..qstart].ends_with('@')
        && is_token_start(before, qstart - 1)
    {
        return Some(before[qstart - 1..].to_owned());
    }
    let start = before
        .rfind([' ', '\t', '"', '\'', '='])
        .map_or(0, |i| i + 1);
    let token = &before[start..];
    (token.starts_with('@') && is_token_start(before, start)).then(|| token.to_owned())
}

fn find_unclosed_quote(text: &str) -> Option<usize> {
    let mut open = None;
    for (i, c) in text.char_indices() {
        if c == '"' {
            open = match open {
                Some(_) => None,
                None => Some(i),
            };
        }
    }
    open
}

fn is_token_start(text: &str, index: usize) -> bool {
    index == 0
        || matches!(
            text.as_bytes()[index - 1],
            b' ' | b'\t' | b'"' | b'\'' | b'='
        )
}

/// Collapse a message to one display line for selector rows.
fn single_line(text: &str) -> String {
    let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.is_empty() {
        "(empty)".to_owned()
    } else {
        joined
    }
}

/// Path-like token ending at the cursor for Tab completion: the last
/// whitespace-delimited word (always returned; Tab forces completion).
fn extract_path_token(before: &str) -> String {
    let start = before.rfind([' ', '\t']).map_or(0, |i| i + 1);
    before[start..].to_owned()
}

pub async fn run(setup: TuiSetup, mut io: SessionIo) -> Result<()> {
    let TuiSetup {
        rlm,
        payload,
        mut events,
        missing_credentials,
        ui: ui_setup,
    } = setup;

    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<Cmd>();
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<Done>();
    let (login_tx, mut login_rx) = mpsc::unbounded_channel::<LoginMsg>();

    let session_task = tokio::spawn(async move {
        let mut session = rlm.into_session(payload);
        while let Some(cmd) = cmd_rx.recv().await {
            match cmd {
                Cmd::Prompt { text, cancel } => {
                    let completion = session.send(&text, &cancel).await;
                    if done_tx.send(Done::Run(completion)).is_err() {
                        break;
                    }
                }
                Cmd::Shell { code, record } => {
                    let result = session.exec_shell(&code, record).await;
                    if done_tx.send(Done::Shell { code, record, result }).is_err() {
                        break;
                    }
                }
                Cmd::Reset => session.reset().await,
                Cmd::Restore { exchanges, pending } => {
                    let result = session.restore_session(&exchanges, &pending).await;
                    if done_tx.send(Done::Restored(result)).is_err() {
                        break;
                    }
                }
                Cmd::Shutdown => break,
            }
        }
        session.shutdown().await;
    });

    let guard = if io.embedded {
        None
    } else {
        Some(RawModeGuard::new()?)
    };
    let (width, height) = io
        .dimensions
        .unwrap_or_else(|| phi_tui::crossterm::terminal::size().unwrap_or((80, 24)));
    let writer = io
        .writer
        .take()
        .unwrap_or_else(|| Box::new(std::io::stdout()));
    let mut ui = Ui::new(
        Terminal::new(writer, width, height),
        ui_setup,
        login_tx,
        io.cwd.clone(),
        io.host_modal_tx.take(),
    );
    let restoring = ui.store.leaf().is_some();
    if restoring {
        ui.replay_branch();
        ui.restore_rlm(&cmd_tx);
    }
    let mut input = match io.event_rx.take() {
        Some(rx) => InputSource::Forwarded(rx),
        None => InputSource::Tty(EventStream::new()),
    };
    let mut ticker = tokio::time::interval(Duration::from_millis(100));
    // Coalesce redraws to ~60fps (pi-style): bursts of stream deltas collapse
    // into one frame instead of repainting per SSE chunk.
    const FRAME: Duration = Duration::from_millis(16);
    let mut last_draw = tokio::time::Instant::now();
    let mut dirty = false;

    // Terminal title (OSC 0), pi-style.
    if !io.embedded {
        use std::io::Write as _;
        let mut out = std::io::stdout();
        let _ = write!(out, "\x1b]0;phi · {}\x07", display_path(&ui.cwd));
        let _ = out.flush();
    }

    if !restoring {
        ui.greet(&missing_credentials);
    }
    ui.draw()?;

    loop {
        let quit = tokio::select! {
            maybe = input.next() => match maybe {
                #[cfg(unix)]
                Some(Ok(Event::Key(key)))
                    if guard.is_some()
                        && !io.embedded
                        && key.kind != KeyEventKind::Release
                        && key.code == KeyCode::Char('z')
                        && key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    let _ = ui.term.finish();
                    if let Some(guard) = &guard {
                        guard.suspend();
                    }
                    false
                }
                #[cfg(unix)]
                Some(Ok(Event::Key(key)))
                    if key.kind != KeyEventKind::Release
                        && key.code == KeyCode::Char('z')
                        && key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    false
                }
                Some(Ok(event)) => ui.handle_input(event, &cmd_tx),
                Some(Err(_)) | None => true,
            },
            Some(event) = events.recv() => {
                ui.handle_rlm_event(event);
                false
            }
            Some(completion) = done_rx.recv() => {
                ui.handle_done(completion, &cmd_tx);
                false
            }
            Some(msg) = login_rx.recv() => {
                ui.handle_login_msg(msg);
                false
            }
            _ = ticker.tick(), if ui.running.is_some() => {
                ui.spinner.tick();
                false
            }
            _ = tokio::time::sleep_until(last_draw + FRAME), if dirty => false,
        };
        if quit {
            break;
        }
        if ui.external_edit {
            ui.external_edit = false;
            // Stop the event reader and hand the tty to the editor; the
            // cleared live region forces a full repaint afterwards.
            let _ = ui.term.finish();
            let text = ui.editor.expanded_text();
            let result = if let Some(guard) = &guard {
                drop(std::mem::replace(&mut input, InputSource::Closed));
                let result = guard.with_suspended(|| external_editor(&text));
                input = InputSource::Tty(EventStream::new());
                result
            } else if let Some(suspend_tx) = &io.suspend_tx {
                let (response_tx, response_rx) = oneshot::channel();
                if suspend_tx
                    .send(SuspendRequest { text, response_tx })
                    .is_err()
                {
                    Err("external editor unavailable".to_owned())
                } else {
                    response_rx
                        .await
                        .unwrap_or_else(|_| Err("external editor request cancelled".to_owned()))
                }
            } else {
                Err("external editor unavailable".to_owned())
            };
            match result {
                Ok(Some(new_text)) => {
                    ui.editor.set_text(new_text.trim_end_matches('\n'));
                    ui.refresh_popup();
                }
                Ok(None) => {}
                Err(e) => ui.pending.push(format!("\x1b[31merror:{RESET} {e}")),
            }
        }
        if let Some(pending_modal) = ui.pending_host_modal.take() {
            let kind = pending_modal.kind.clone();
            let result = pending_modal
                .response_rx
                .await
                .unwrap_or_else(|_| Err("host modal request cancelled".to_owned()));
            match result {
                Ok(Some(value)) => {
                    if let HostModalKind::Model { root, .. } = kind {
                        ui.switch_model(&value, root);
                    }
                }
                Ok(None) => {}
                Err(e) => ui.pending.push(format!("\x1b[31merror:{RESET} {e}")),
            }
        }
        dirty = true;
        if last_draw.elapsed() >= FRAME {
            ui.draw()?;
            last_draw = tokio::time::Instant::now();
            dirty = false;
        }
    }

    if let Some(login) = &ui.login {
        login.cancel.cancel();
    }
    if let Some(cancel) = &ui.running {
        cancel.cancel();
    }
    let _ = cmd_tx.send(Cmd::Shutdown);
    ui.term.finish()?;
    drop(guard);
    let _ = tokio::time::timeout(Duration::from_secs(5), session_task).await;
    if !io.embedded {
        println!("bye");
    }
    Ok(())
}

/// Write `text` to a temp file, open it in $VISUAL/$EDITOR, and return the
/// edited content (None when the editor exits non-zero).
pub(crate) fn external_editor(text: &str) -> Result<Option<String>, String> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .map_err(|_| "set $VISUAL or $EDITOR to use ctrl+g".to_owned())?;
    let path = std::env::temp_dir().join(format!("phi-edit-{}.md", std::process::id()));
    std::fs::write(&path, text).map_err(|e| e.to_string())?;
    let mut parts = editor.split_whitespace();
    let cmd = parts.next().ok_or_else(|| "$EDITOR is empty".to_owned())?;
    let status = std::process::Command::new(cmd)
        .args(parts)
        .arg(&path)
        .status()
        .map_err(|e| format!("failed to launch '{editor}': {e}"));
    let result = match status {
        Ok(s) if s.success() => std::fs::read_to_string(&path)
            .map(Some)
            .map_err(|e| e.to_string()),
        Ok(_) => Ok(None),
        Err(e) => Err(e),
    };
    let _ = std::fs::remove_file(&path);
    result
}

struct LoginState {
    code_tx: mpsc::UnboundedSender<String>,
    cancel: CancellationToken,
}

struct TuiLoginCallbacks {
    msg_tx: mpsc::UnboundedSender<LoginMsg>,
    code_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<String>>,
}

impl LoginCallbacks for TuiLoginCallbacks {
    fn on_auth_url(&self, url: &str, instructions: &str) {
        let _ = self.msg_tx.send(LoginMsg::Info(instructions.to_owned()));
        let _ = self.msg_tx.send(LoginMsg::Info(format!("  {url}")));
    }

    fn on_device_code(&self, user_code: &str, verification_uri: &str, expires_in: Duration) {
        let _ = self.msg_tx.send(LoginMsg::Info(format!(
            "Visit {verification_uri} and enter code: {user_code} (expires in {} minutes)",
            expires_in.as_secs() / 60
        )));
    }

    fn on_progress(&self, message: &str) {
        let _ = self.msg_tx.send(LoginMsg::Info(message.to_owned()));
    }

    fn manual_code(&self) -> std::pin::Pin<Box<dyn Future<Output = Option<String>> + Send + '_>> {
        Box::pin(async move { self.code_rx.lock().await.recv().await })
    }
}

struct Ui {
    term: Terminal<Box<dyn std::io::Write + Send>>,
    editor: Editor,
    spinner: Spinner,
    /// Cancel token of the in-flight turn, if any.
    running: Option<CancellationToken>,
    /// In-flight /login flow, if any. The editor feeds it pasted codes.
    login: Option<LoginState>,
    /// Slash command autocomplete, shown below the editor.
    popup: Option<Popup>,
    /// Active full-width selector (model/resume/fork/tree); replaces the
    /// editor area and captures all keys until confirmed or cancelled.
    selector: Option<(SelectorKind, Selector)>,
    /// On-disk session store recording the conversation tree.
    store: SessionStore,
    /// Time of the last esc on an idle, empty editor; a second one within
    /// 500ms opens the session tree (pi's double-escape).
    last_esc: Option<std::time::Instant>,
    /// Worktree file search backing `@` completion.
    files: FileSearcher,
    cwd: std::path::PathBuf,
    /// First visible logical editor line (viewport scrolling).
    editor_scroll: usize,
    /// Set by ctrl+g; the run loop opens $VISUAL/$EDITOR on the buffer.
    external_edit: bool,
    /// ctrl+o: show full code/output instead of previews. Scrollback is
    /// permanent, so the toggle affects output from now on (unlike pi,
    /// which re-renders retained components).
    expand_tools: bool,
    /// ctrl+t: show thinking blocks from now on. Like tool expansion, old
    /// scrollback is permanent, so toggling only affects the current live
    /// block and future output.
    expand_thinking: bool,
    /// Follow-up prompts queued while a turn is in flight; sent FIFO as
    /// turns complete.
    queued: Vec<String>,
    /// Time of the last ctrl-c; a second one within 500ms quits.
    last_ctrl_c: Option<std::time::Instant>,
    /// Lines to flush into scrollback on the next draw.
    pending: Vec<String>,
    /// Rows of the in-flight depth-0 REPL block (with everything nested in
    /// it), shown live with the pending background and committed to
    /// scrollback with the success/error background when the exec ends.
    /// A row tagged with its own background (a finished nested tool) keeps
    /// it instead of the block's.
    block: Vec<(String, Option<&'static str>)>,
    block_active: bool,
    /// Code of the in-flight depth-0 agent REPL block, stashed at
    /// `CodeExecStart` and persisted to the session store at `CodeExecEnd`.
    live_code: Option<String>,
    /// Tool calls observed inside the in-flight depth-0 REPL block; args
    /// stashed at `ToolStart`, completed at `ToolEnd`, persisted with the
    /// block at `CodeExecEnd` so resume re-renders the same tool cards.
    live_tools: Vec<session::ToolCall>,
    /// Args of depth-0 tools still executing, keyed by engine call_id.
    open_tool_args: std::collections::HashMap<u64, serde_json::Value>,
    /// Sub-LLM calls observed inside the in-flight depth-0 REPL block;
    /// prompt previews queue at `SubcallStart` and pair with `SubcallEnd`
    /// in FIFO order (the engine emits no subcall ids).
    live_subcalls: Vec<session::SubcallRecord>,
    open_subcall_previews: std::collections::VecDeque<String>,
    /// Index into `block` of the first row of the nested tool currently
    /// executing; its rows are tinted with the tool background on `ToolEnd`.
    tool_rows_from: Option<usize>,
    /// Prose flushed while a block is active; committed after the block so
    /// scrollback stays chronological.
    after_block: Vec<String>,
    /// Whether the last scrollback line was gutter activity; a blank line is
    /// inserted before a depth-0 action that follows prose.
    last_gutter: bool,
    /// Streamed root-model text for the current iteration, minus the
    /// paragraphs/fences already flushed to scrollback.
    stream: String,
    /// Streamed provider thinking for the current iteration.
    thinking: String,
    /// Whether any `RootDelta` arrived this iteration; when false the model
    /// did not stream and `RootText` carries the only copy of the text.
    streamed: bool,
    status: String,
    turn: usize,
    max_iterations: usize,
    model_label: String,
    storage: Arc<AuthStorage>,
    registry: Arc<Registry>,
    root_lm: Arc<ProviderLm>,
    sub_lm: Arc<ProviderLm>,
    /// Qualified `provider/id` names offered by /model completion.
    model_names: Vec<String>,
    login_tx: mpsc::UnboundedSender<LoginMsg>,
    host_modal_tx: Option<mpsc::UnboundedSender<HostModalRequest>>,
    pending_host_modal: Option<PendingHostModal>,
    /// Usage of the most recent completed run (footer stats).
    usage: Usage,
    /// Cached git branch for the footer, refreshed on a short TTL.
    branch: Option<String>,
    branch_at: std::time::Instant,
}

/// Current git branch of `cwd` (or a short detached-HEAD hash), following a
/// worktree `gitdir:` redirect.
fn git_branch(cwd: &std::path::Path) -> Option<String> {
    let dotgit = cwd.join(".git");
    let git_dir = if dotgit.is_file() {
        let redirect = std::fs::read_to_string(&dotgit).ok()?;
        let path = redirect.trim().strip_prefix("gitdir: ")?.to_owned();
        let path = std::path::PathBuf::from(path);
        if path.is_absolute() {
            path
        } else {
            cwd.join(path)
        }
    } else {
        dotgit
    };
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    match head.strip_prefix("ref: ") {
        Some(reference) => Some(reference.rsplit('/').next()?.to_owned()),
        None => Some(head.get(..8)?.to_owned()),
    }
}

/// Abbreviate `$HOME` to `~` for display.
fn display_path(path: &std::path::Path) -> String {
    let s = path.display().to_string();
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && s.starts_with(&home) => format!("~{}", &s[home.len()..]),
        _ => s,
    }
}

fn model_label(root: &Model, sub: &Model) -> String {
    if root.provider == sub.provider && root.id == sub.id {
        root.id.clone()
    } else {
        format!("{} · sub {}", root.id, sub.id)
    }
}

impl Ui {
    fn new(
        term: Terminal<Box<dyn std::io::Write + Send>>,
        setup: UiSetup,
        login_tx: mpsc::UnboundedSender<LoginMsg>,
        cwd: PathBuf,
        host_modal_tx: Option<mpsc::UnboundedSender<HostModalRequest>>,
    ) -> Self {
        let UiSetup {
            max_iterations,
            storage,
            registry,
            root_lm,
            sub_lm,
            store,
        } = setup;
        let model_names = registry
            .supported_models()
            .map(|m| format!("{}/{}", m.provider, m.id))
            .collect();
        Self {
            term,
            editor: Editor::new(),
            spinner: Spinner::new(),
            running: None,
            login: None,
            popup: None,
            selector: None,
            store,
            last_esc: None,
            files: FileSearcher::new(cwd.clone()),
            cwd,
            editor_scroll: 0,
            external_edit: false,
            expand_tools: false,
            expand_thinking: false,
            queued: Vec::new(),
            last_ctrl_c: None,
            pending: Vec::new(),
            block: Vec::new(),
            block_active: false,
            live_code: None,
            live_tools: Vec::new(),
            open_tool_args: std::collections::HashMap::new(),
            live_subcalls: Vec::new(),
            open_subcall_previews: std::collections::VecDeque::new(),
            tool_rows_from: None,
            after_block: Vec::new(),
            last_gutter: false,
            stream: String::new(),
            thinking: String::new(),
            streamed: false,
            status: String::new(),
            turn: 0,
            max_iterations,
            model_label: model_label(&root_lm.model(), &sub_lm.model()),
            storage,
            registry,
            root_lm,
            sub_lm,
            model_names,
            login_tx,
            host_modal_tx,
            pending_host_modal: None,
            usage: Usage::default(),
            branch: None,
            branch_at: std::time::Instant::now() - Duration::from_secs(60),
        }
    }

    fn greet(&mut self, missing_credentials: &[String]) {
        self.pending.push(format!(
            "{BOLD}phi{RESET} {DIM}· {} · /help for commands{RESET}",
            self.model_label
        ));
        for provider in missing_credentials {
            self.pending.push(format!(
                "{YELLOW}no credentials for '{provider}'{RESET} {DIM}— type /login to authenticate{RESET}"
            ));
        }
    }

    fn draw(&mut self) -> std::io::Result<()> {
        let scrollback = std::mem::take(&mut self.pending);
        let (live, cursor) = self.live_lines();
        self.term.render(&scrollback, &live, cursor)
    }

    fn live_lines(&mut self) -> (Vec<String>, Option<LiveCursor>) {
        let mut lines: Vec<String> = Vec::new();

        // While a turn is in flight, the unflushed stream tail renders as
        // live markdown above the status footer, capped so the whole live
        // region (tail + status + editor + queue + hints) fits the viewport.
        // Anything that is (or could still become) a ```repl fence is kept
        // out of markdown so it never appears as mangled prose. Once the
        // opening fence lands, render a provisional tool block immediately;
        // `CodeExecStart` takes over in scrollback when the block closes.
        if self.running.is_some() {
            let bg_width = self.term.width();
            let visible = &self.stream[..first_block_start(&self.stream)];
            let live_block = live_block_body(&self.stream);
            let mut tail = self.live_thinking_lines();
            // The executing block renders live over the pending background,
            // followed by any prose already flushed behind it.
            if self.block_active {
                if !tail.is_empty() {
                    tail.push(String::new());
                }
                tail.extend(
                    self.block
                        .iter()
                        .map(|(row, row_bg)| fill_bg(row, row_bg.unwrap_or(REPL_BG), bg_width)),
                );
                tail.extend(self.after_block.iter().cloned());
            }
            let mut prose = render_markdown(visible.trim(), self.markdown_width());
            if !tail.is_empty() && (!prose.is_empty() || live_block.is_some()) {
                tail.push(String::new());
            }
            tail.append(&mut prose);
            if let Some(block) = live_block {
                if !tail.is_empty() {
                    tail.push(String::new());
                }
                tail.extend(
                    self.tool_lines(0, &format!("{BOLD}repl{RESET}"), block, self.code_limit())
                        .iter()
                        .map(|row| fill_bg(row, REPL_BG, bg_width)),
                );
            }
            let editor_visible = self
                .editor
                .render()
                .0
                .len()
                .min((self.term.height() * 3 / 10).max(5));
            let reserved =
                5 + editor_visible + self.queued.len() + usize::from(!self.queued.is_empty());
            let budget = self.term.height().saturating_sub(reserved).max(1);
            if tail.len() > budget {
                tail.drain(..tail.len() - budget);
            }
            lines.extend(tail);
            lines.push(format!(
                "{CYAN}{}{RESET} {DIM}{} · turn {}/{}{RESET}",
                self.spinner.frame(),
                self.status,
                self.turn,
                self.max_iterations
            ));
        }

        let width = self.term.width();

        // Persistent footer chrome: contrast line first, then cwd/branch.
        if self.branch_at.elapsed() > Duration::from_secs(5) {
            self.branch = git_branch(&self.cwd);
            self.branch_at = std::time::Instant::now();
        }
        let mut pwd = display_path(&self.cwd);
        if let Some(branch) = &self.branch {
            pwd.push_str(&format!(" ({branch})"));
        }
        lines.push(format!("{RESET}{}{RESET}", "─".repeat(width)));
        lines.push(format!("{DIM}{pwd}{RESET}"));

        let cursor;
        if let Some((_, widget)) = &self.selector {
            // Selector replaces the editor area below the cwd line.
            let (sel_lines, sel_cursor) = widget.render(width);
            let top = lines.len();
            cursor = sel_cursor.map(|(line, col)| LiveCursor {
                line: top + line,
                col,
            });
            lines.extend(sel_lines);
            let hint = if widget.searchable() {
                "type to filter · up/down select · enter confirm · esc cancel"
            } else {
                "up/down select · enter confirm · esc cancel"
            };
            lines.push(format!("{DIM}{hint}{RESET}"));
        } else {
            // Editor viewport: at most 30% of the terminal (min 5 lines),
            // scrolled to keep the cursor visible. No surrounding rails; the
            // first line keeps the bold prompt marker and continuations stay
            // minimally indented.
            let (editor_lines, (row, col)) = self.editor.render();
            let max_visible = (self.term.height() * 3 / 10).max(5);
            if row < self.editor_scroll {
                self.editor_scroll = row;
            } else if row + 1 > self.editor_scroll + max_visible {
                self.editor_scroll = row + 1 - max_visible;
            }
            self.editor_scroll = self
                .editor_scroll
                .min(editor_lines.len().saturating_sub(max_visible));
            let end = (self.editor_scroll + max_visible).min(editor_lines.len());

            let editor_top = lines.len();
            for (i, line) in editor_lines[self.editor_scroll..end].iter().enumerate() {
                if i + self.editor_scroll == 0 {
                    if line.is_empty() {
                        lines.push(format!("{BOLD}›{RESET}"));
                    } else {
                        lines.push(format!("{BOLD}›{RESET} {line}"));
                    }
                } else {
                    lines.push(format!("  {line}"));
                }
            }
            if end < editor_lines.len() {
                let below = editor_lines.len() - end;
                lines.push(format!("{DIM}↓ {below} more{RESET}"));
            }
            let prompt_cols = if row == 0 && editor_lines.first().is_some_and(|line| line.is_empty()) {
                1
            } else {
                2
            };
            cursor = Some(LiveCursor {
                line: editor_top + (row - self.editor_scroll),
                col: col + prompt_cols,
            });

            for text in &self.queued {
                let first = text.lines().next().unwrap_or("");
                let more = if text.lines().count() > 1 { " …" } else { "" };
                lines.push(format!("{DIM}queued › {first}{more}{RESET}"));
            }

            if let Some(popup) = &self.popup {
                lines.extend(render_popup(popup));
            }

            // Contextual hints, shown only when there is a mode to explain.
            let hints: &[&str] = if self.login.is_some() {
                &[
                    "finish login in the browser, or paste the redirect URL and press enter · esc cancels",
                ]
            } else if self.popup.is_some() {
                &["tab complete · up/down select · enter accept · esc dismiss"]
            } else if self.running.is_some() && self.queued.is_empty() {
                &["enter queue follow-up · esc cancel"]
            } else if self.running.is_some() {
                &["enter queue follow-up", "alt+up edit queued · esc cancel"]
            } else {
                &[]
            };
            for hint in hints {
                lines.push(format!("{DIM}{hint}{RESET}"));
            }
        }

        let left = if self.usage.total_tokens > 0 {
            format!(
                "↑{} ↓{} ${:.4}",
                format_tokens(self.usage.input),
                format_tokens(self.usage.output),
                self.usage.cost.total
            )
        } else {
            String::new()
        };
        let mut right = format!(
            "{} · thinking:{}",
            self.model_label,
            self.root_lm.thinking().as_str()
        );
        if self.expand_thinking {
            right.push_str(" · thoughts:show");
        }
        if self.expand_tools {
            right.push_str(" · tools:full");
        }
        let footer = match (left.is_empty(), right.is_empty()) {
            (true, true) => String::new(),
            (true, false) => right,
            (false, true) => left,
            (false, false) => format!("{left} · {right}"),
        };
        lines.push(format!("{DIM}{footer}{RESET}"));
        (lines, cursor)
    }

    /// Popup key handling; pi-style: up/down select, tab applies, enter
    /// applies (and for command names falls through to submit), esc
    /// dismisses. Returns the action the caller should take.
    fn handle_popup_key(&mut self, key: phi_tui::crossterm::event::KeyEvent) -> PopupOutcome {
        let Some(popup) = &mut self.popup else {
            return PopupOutcome::Pass;
        };
        match key.code {
            KeyCode::Esc => {
                self.popup = None;
                PopupOutcome::Handled
            }
            KeyCode::Up | KeyCode::BackTab => {
                popup.selected = popup
                    .selected
                    .checked_sub(1)
                    .unwrap_or(popup.items.len() - 1);
                PopupOutcome::Handled
            }
            KeyCode::Down => {
                popup.selected = (popup.selected + 1) % popup.items.len();
                PopupOutcome::Handled
            }
            KeyCode::Tab => {
                self.apply_completion();
                PopupOutcome::Handled
            }
            KeyCode::Enter if !key.modifiers.contains(KeyModifiers::ALT) => {
                let kind = popup.kind;
                self.apply_completion();
                match kind {
                    PopupKind::Command => PopupOutcome::Pass,
                    PopupKind::Argument | PopupKind::File | PopupKind::Path => {
                        PopupOutcome::Handled
                    }
                }
            }
            _ => PopupOutcome::Pass,
        }
    }

    fn apply_completion(&mut self) {
        let Some(popup) = self.popup.take() else {
            return;
        };
        let item = &popup.items[popup.selected];
        // Directory completions stay open for further completion (pi-style);
        // file completions get a trailing space to end the token.
        let is_dir = item.value.trim_end_matches('"').ends_with('/');
        let replacement = match popup.kind {
            PopupKind::Command => format!("/{} ", item.value),
            PopupKind::Argument => item.value.clone(),
            PopupKind::File if !is_dir => format!("{} ", item.value),
            PopupKind::File | PopupKind::Path => item.value.clone(),
        };
        let kind = popup.kind;
        self.editor.replace_prefix(&popup.prefix, &replacement);
        if is_dir {
            match kind {
                PopupKind::File => self.refresh_popup(),
                PopupKind::Path => self.open_path_popup(),
                _ => {}
            }
        }
    }

    /// Tab with no popup open: complete the path-like token at the cursor.
    fn open_path_popup(&mut self) {
        let before = self.editor.before_cursor().to_owned();
        let prefix = extract_path_token(&before);
        let items: Vec<Suggestion> = files::complete_path(&self.cwd, &prefix)
            .into_iter()
            .map(|(value, label)| Suggestion {
                value,
                label,
                desc: String::new(),
            })
            .collect();
        if !items.is_empty() {
            self.popup = Some(Popup {
                items,
                selected: 0,
                prefix,
                kind: PopupKind::Path,
            });
        }
    }

    fn open_selector(&mut self, kind: SelectorKind, widget: Selector) {
        self.popup = None;
        self.selector = Some((kind, widget));
    }

    fn request_host_model_modal(&mut self, root: bool) -> bool {
        let Some(host_modal_tx) = &self.host_modal_tx else {
            return false;
        };
        let current = {
            let m = if root {
                self.root_lm.model()
            } else {
                self.sub_lm.model()
            };
            format!("{}/{}", m.provider, m.id)
        };
        let kind = HostModalKind::Model {
            root,
            current: current.clone(),
        };
        let (response_tx, response_rx) = oneshot::channel();
        if host_modal_tx
            .send(HostModalRequest {
                kind: kind.clone(),
                response_tx,
            })
            .is_err()
        {
            return false;
        }
        self.pending_host_modal = Some(PendingHostModal { kind, response_rx });
        true
    }

    fn request_host_usage_modal(&mut self) -> bool {
        let Some(host_modal_tx) = &self.host_modal_tx else {
            return false;
        };
        let kind = HostModalKind::Usage;
        let (response_tx, response_rx) = oneshot::channel();
        if host_modal_tx
            .send(HostModalRequest {
                kind: kind.clone(),
                response_tx,
            })
            .is_err()
        {
            return false;
        }
        self.pending_host_modal = Some(PendingHostModal { kind, response_rx });
        true
    }

    fn open_usage_selector(&mut self) {
        let report = load_usage_report();
        let mut items = vec![SelectorItem::new(
            "__usage_total__",
            format!(
                "entries {} · files {} · total {} tok · ${:.4}",
                report.entries,
                report.files_scanned,
                format_tokens(report.totals.total_tokens()),
                report.totals.total_cost
            ),
        )];
        for (index, line) in usage_overlay_lines(&report).into_iter().enumerate() {
            items.push(SelectorItem::new(format!("usage-{index}"), line));
        }
        let widget = Selector::new(
            format!("Usage · {} entries", report.entries),
            items,
            false,
        );
        self.open_selector(SelectorKind::Usage, widget);
    }

    /// /model, /submodel, ctrl-l: full-width model selector (pi-style),
    /// preselected at the active model.
    fn open_model_selector(&mut self, root: bool) {
        if self.model_names.is_empty() {
            return;
        }
        let current = {
            let m = if root {
                self.root_lm.model()
            } else {
                self.sub_lm.model()
            };
            format!("{}/{}", m.provider, m.id)
        };
        let items = self
            .model_names
            .iter()
            .map(|name| {
                let (provider, id) = name.split_once('/').unwrap_or(("", name));
                SelectorItem::new(name.clone(), id.to_owned())
                    .detail(format!("[{provider}]"))
                    .current(*name == current)
            })
            .collect();
        let title = if root {
            "Switch Model"
        } else {
            "Switch Sub Model"
        };
        let mut widget = Selector::new(title, items, true);
        widget.preselect(&current);
        self.open_selector(SelectorKind::Model { root }, widget);
    }

    /// Guard for session selectors: they rebuild env history, which would
    /// race an in-flight turn or login.
    fn session_idle(&mut self) -> bool {
        if self.running.is_some() || self.login.is_some() {
            self.pending
                .push(format!("{DIM}[wait for the current run to finish]{RESET}"));
            return false;
        }
        true
    }

    /// /resume: pick a past session of this directory.
    fn open_resume_selector(&mut self) {
        if !self.session_idle() {
            return;
        }
        let current = self.store.path().to_owned();
        let items: Vec<SelectorItem> = SessionStore::list(&self.cwd)
            .into_iter()
            .filter(|s| s.path != current)
            .map(|s| {
                SelectorItem::new(s.path.display().to_string(), single_line(&s.first_message))
                    .detail(format!(
                        "{} msgs · {}",
                        s.message_count,
                        session::age(s.modified)
                    ))
            })
            .collect();
        if items.is_empty() {
            self.pending.push(format!(
                "{DIM}[no saved sessions for this directory]{RESET}"
            ));
            return;
        }
        self.open_selector(
            SelectorKind::Resume,
            Selector::new("Resume Session", items, true),
        );
    }

    /// /fork: pick a prior user message (any branch) to fork into a new
    /// session; its text returns to the editor.
    fn open_fork_selector(&mut self) {
        if !self.session_idle() {
            return;
        }
        let messages = self.store.user_messages();
        if messages.is_empty() {
            self.pending
                .push(format!("{DIM}[no messages to fork from]{RESET}"));
            return;
        }
        let total = messages.len();
        let items = messages
            .iter()
            .enumerate()
            .map(|(i, e)| {
                SelectorItem::new(e.id.clone(), single_line(&e.text))
                    .detail(format!("{}/{total}", i + 1))
            })
            .collect();
        let last = messages.last().expect("non-empty").id.clone();
        let mut widget = Selector::new("Fork from Message", items, false);
        widget.preselect(&last);
        self.open_selector(SelectorKind::Fork, widget);
    }


    /// /tree (or esc esc): navigate the whole session tree.
    fn open_tree_selector(&mut self) {
        if !self.session_idle() {
            return;
        }
        let leaf = self.store.leaf().map(str::to_owned);
        let items: Vec<SelectorItem> = self
            .store
            .tree_rows()
            .into_iter()
            .map(|(depth, entry)| {
                let marker = match entry.role {
                    session::Role::User => "›",
                    session::Role::Assistant => "·",
                };
                SelectorItem::new(
                    entry.id.clone(),
                    format!(
                        "{}{marker} {}",
                        "  ".repeat(depth),
                        single_line(&entry.text)
                    ),
                )
                .current(Some(entry.id.as_str()) == leaf.as_deref())
            })
            .collect();
        if items.is_empty() {
            self.pending
                .push(format!("{DIM}[no entries in this session]{RESET}"));
            return;
        }
        let mut widget = Selector::new("Session Tree", items, false);
        if let Some(leaf) = &leaf {
            widget.preselect(leaf);
        }
        self.open_selector(SelectorKind::Tree, widget);
    }

    /// Route a key to the open selector; consumes every key until the
    /// selector closes.
    fn handle_selector_key(
        &mut self,
        key: phi_tui::crossterm::event::KeyEvent,
        cmd_tx: &mpsc::UnboundedSender<Cmd>,
    ) {
        let Some((_, widget)) = self.selector.as_mut() else {
            return;
        };
        match widget.handle_key(key) {
            SelectorOutcome::Pending => {}
            SelectorOutcome::Cancelled => self.selector = None,
            SelectorOutcome::Confirmed(index) => {
                let (kind, widget) = self.selector.take().expect("selector open");
                let value = widget.items()[index].value.clone();
                match kind {
                    SelectorKind::Model { root } => self.switch_model(&value, root),
                    SelectorKind::Usage => {}
                    SelectorKind::Resume => self.resume_session(&value, cmd_tx),
                    SelectorKind::Fork => self.fork_from(&value, cmd_tx),
                    SelectorKind::Tree => self.navigate_tree(&value, cmd_tx),
                }
            }
        }
    }

    fn resume_session(&mut self, path: &str, cmd_tx: &mpsc::UnboundedSender<Cmd>) {
        match SessionStore::load(std::path::Path::new(path)) {
            Ok(store) => {
                self.store = store;
                self.pending.push(String::new());
                self.pending
                    .push(format!("{DIM}[resumed session {}]{RESET}", self.store.id()));
                self.replay_branch();
                self.restore_rlm(cmd_tx);
            }
            Err(e) => self.pending.push(format!("\x1b[31merror:{RESET} {e}")),
        }
    }

    fn fork_from(&mut self, entry_id: &str, cmd_tx: &mpsc::UnboundedSender<Cmd>) {
        let text = self
            .store
            .entry(entry_id)
            .map(|e| e.text.clone())
            .unwrap_or_default();
        match self.store.fork(entry_id) {
            Ok(store) => {
                self.store = store;
                self.pending.push(String::new());
                self.pending
                    .push(format!("{DIM}[forked to new session]{RESET}"));
                self.replay_branch();
                self.restore_rlm(cmd_tx);
                self.editor.set_text(&text);
            }
            Err(e) => self.pending.push(format!("\x1b[31merror:{RESET} {e}")),
        }
    }

    /// Move the active branch to `entry_id` (pi's navigateTree): selecting a
    /// user message rewinds to just before it and returns its text to the
    /// editor; anything else becomes the new branch tip.
    fn navigate_tree(&mut self, entry_id: &str, cmd_tx: &mpsc::UnboundedSender<Cmd>) {
        if self.store.leaf() == Some(entry_id) {
            self.pending
                .push(format!("{DIM}[already at this point]{RESET}"));
            return;
        }
        let Some(entry) = self.store.entry(entry_id) else {
            return;
        };
        let (leaf, editor_text) = match entry.role {
            session::Role::User => (entry.parent_id.clone(), Some(entry.text.clone())),
            session::Role::Assistant => (Some(entry.id.clone()), None),
        };
        self.store.set_leaf(leaf);
        self.pending.push(String::new());
        self.pending
            .push(format!("{DIM}[navigated to selected point]{RESET}"));
        self.replay_branch();
        self.restore_rlm(cmd_tx);
        if let Some(text) = editor_text
            && self.editor.text().trim().is_empty()
        {
            self.editor.set_text(&text);
        }
    }

    /// Rebuild the REPL env history from the active branch.
    fn restore_rlm(&mut self, cmd_tx: &mpsc::UnboundedSender<Cmd>) {
        let (exchanges, pending) = self.store.branch_restore();
        let _ = cmd_tx.send(Cmd::Restore { exchanges, pending });
    }

    /// Re-render the active branch into scrollback, mirroring how prompts
    /// and responses looked live. The screen (and its scrollback) is wiped
    /// first so the replayed transcript replaces the old one instead of
    /// stacking below it.
    fn replay_branch(&mut self) {
        let _ = self.term.clear_all();
        enum ReplayItem {
            Message(session::Role, String),
            Repl {
                code: String,
                stdout: String,
                stderr: String,
                duration_secs: f64,
                tools: Vec<session::ToolCall>,
                subcalls: Vec<session::SubcallRecord>,
            },
        }
        let items: Vec<ReplayItem> = self
            .store
            .branch_items()
            .into_iter()
            .map(|item| match item {
                session::BranchItem::Message(entry) => {
                    ReplayItem::Message(entry.role, entry.text.clone())
                }
                session::BranchItem::Repl(repl) => ReplayItem::Repl {
                    code: repl.code.clone(),
                    stdout: repl.stdout.clone(),
                    stderr: repl.stderr.clone(),
                    duration_secs: repl.duration_secs,
                    tools: repl.tools.clone(),
                    subcalls: repl.subcalls.clone(),
                },
            })
            .collect();
        for item in items {
            match item {
                ReplayItem::Message(role, text) => match role {
                    session::Role::User => self.push_user_message(&text),
                    session::Role::Assistant => self.push_assistant_message(&text),
                },
                ReplayItem::Repl {
                    code,
                    stdout,
                    stderr,
                    duration_secs,
                    tools,
                    subcalls,
                } => self.push_repl_replay(&code, &stdout, &stderr, duration_secs, &tools, &subcalls),
            }
        }
        // A previous run died mid-turn (exchange_start without its
        // exchange_end): say so instead of silently presenting a transcript
        // that ends earlier than the user remembers, then durably close the
        // exchange so the notice appears once.
        if self.store.interrupted_exchange().is_some() {
            self.pending.push(String::new());
            self.pending.push(format!(
                "{YELLOW}previous run was interrupted mid-turn; completed work up to the last checkpoint is shown above{RESET}"
            ));
            self.last_gutter = false;
            let _ = self.store.acknowledge_interrupted();
        }
    }

    fn push_repl_replay(
        &mut self,
        code: &str,
        stdout: &str,
        stderr: &str,
        duration_secs: f64,
        tools: &[session::ToolCall],
        subcalls: &[session::SubcallRecord],
    ) {
        self.begin_block();
        self.push_dot(0, format!("{BOLD}repl{RESET}"));
        self.push_preview(0, code, self.code_limit());
        // Replay the persisted tool calls and sub-LLM calls nested in the
        // block, mirroring the live ToolStart/ToolEnd rendering so a
        // resumed session looks like the one the user actually saw.
        for tool in tools {
            let start = self.block.len();
            self.push_dot(1, tool_title(&tool.name, &tool.args));
            let bg = if tool.error.is_some() {
                TOOL_ERROR_BG
            } else {
                TOOL_SUCCESS_BG
            };
            match &tool.error {
                Some(error) => {
                    self.push_bar(1, format!("failed {:.1}s", tool.duration_secs));
                    self.push_preview(1, error, self.output_limit());
                }
                None => {
                    self.push_bar(1, format!("done {:.1}s", tool.duration_secs));
                    self.push_preview(1, &tool.result, self.output_limit());
                }
            }
            for (_, row_bg) in &mut self.block[start..] {
                *row_bg = Some(bg);
            }
        }
        for subcall in subcalls {
            self.push_dot(
                0,
                format!("{BOLD}{}{RESET} ← \"{}\"", subcall.model, subcall.prompt_preview),
            );
            let text = match &subcall.error {
                Some(error) => format!("{} failed: {error}", subcall.model),
                None => format!("{} done {:.1}s", subcall.model, subcall.duration_secs),
            };
            self.push_bar(0, text);
        }
        self.push_bar(0, format!("exec {duration_secs:.1}s"));
        self.push_preview(0, stdout, self.output_limit());
        self.push_preview(0, stderr, self.output_limit());
        let bg = if stderr.trim().is_empty() {
            REPL_BG
        } else {
            REPL_ERROR_BG
        };
        self.commit_block(bg);
    }

    /// Render a persisted assistant message the way it looked live: prose as
    /// markdown, with each ```repl block shown as a committed repl container
    /// (code preview over the repl background) rather than raw fenced text.
    fn push_assistant_message(&mut self, text: &str) {
        let blocks = find_code_blocks(text);
        let segments = split_prose(text);
        for (i, segment) in segments.iter().enumerate() {
            self.push_prose(segment);
            if let Some(code) = blocks.get(i) {
                self.begin_block();
                self.push_dot(0, format!("{BOLD}repl{RESET}"));
                self.push_preview(0, code, self.code_limit());
                self.commit_block(REPL_BG);
            }
        }
    }

    /// shift+tab: cycle the root model's thinking level (footer shows it).
    fn cycle_thinking(&mut self) {
        let current = self.root_lm.thinking();
        let idx = ThinkingLevel::ALL
            .iter()
            .position(|l| *l == current)
            .unwrap_or(0);
        let next = ThinkingLevel::ALL[(idx + 1) % ThinkingLevel::ALL.len()];
        self.root_lm.set_thinking(next);
    }

    /// ctrl+t: collapse or expand provider thinking blocks from now on.
    fn toggle_thinking(&mut self) {
        self.expand_thinking = !self.expand_thinking;
        self.pending.push(format!(
            "{DIM}[thinking {} from here on]{RESET}",
            if self.expand_thinking {
                "expanded"
            } else {
                "collapsed"
            }
        ));
    }

    /// ctrl-p / ctrl-shift-p: cycle the root model through the registry.
    fn cycle_model(&mut self, back: bool) {
        if self.model_names.is_empty() {
            return;
        }
        let current = {
            let m = self.root_lm.model();
            format!("{}/{}", m.provider, m.id)
        };
        let len = self.model_names.len();
        let next = match self.model_names.iter().position(|n| *n == current) {
            Some(i) if back => (i + len - 1) % len,
            Some(i) => (i + 1) % len,
            None => 0,
        };
        let name = self.model_names[next].clone();
        self.switch_model(&name, true);
    }

    /// Recompute suggestions from the editor; called after the text changes.
    /// Slash commands complete on the first line; `@` file tokens complete
    /// anywhere (pi-style).
    fn refresh_popup(&mut self) {
        self.popup = None;
        if self.login.is_some() {
            return;
        }
        let before = self.editor.before_cursor().to_owned();
        if self.editor.cursor_row() == 0
            && let Some(popup) = complete(&before, &self.model_names)
        {
            self.popup = Some(popup);
            return;
        }
        let Some(token) = extract_at_token(&before) else {
            return;
        };
        let query = token.trim_start_matches('@').trim_start_matches('"');
        let items: Vec<Suggestion> = self
            .files
            .search(query)
            .into_iter()
            .map(|c| Suggestion {
                value: c.value,
                label: c.label,
                desc: c.desc,
            })
            .collect();
        if !items.is_empty() {
            self.popup = Some(Popup {
                items,
                selected: 0,
                prefix: token,
                kind: PopupKind::File,
            });
        }
    }

    /// Move queued follow-ups back into the editor (above any draft text).
    fn restore_queued(&mut self) {
        if self.queued.is_empty() {
            return;
        }
        let mut text = self.queued.join("\n");
        let draft = self.editor.text();
        if !draft.trim().is_empty() {
            text.push('\n');
            text.push_str(&draft);
        }
        self.queued.clear();
        self.editor.set_text(&text);
    }

    /// Returns true when the app should quit.
    fn handle_input(&mut self, event: Event, cmd_tx: &mpsc::UnboundedSender<Cmd>) -> bool {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if self.selector.is_some() {
                    self.handle_selector_key(key, cmd_tx);
                    return false;
                }
                if self.login.is_none() && self.handle_popup_key(key) == PopupOutcome::Handled {
                    return false;
                }
                if key.code == KeyCode::Esc {
                    // Idle esc on an empty editor: a second esc within 500ms
                    // opens the session tree (pi's double-escape).
                    if self.running.is_none()
                        && self.login.is_none()
                        && self.editor.text().trim().is_empty()
                    {
                        let now = std::time::Instant::now();
                        if self
                            .last_esc
                            .replace(now)
                            .is_some_and(|t| now.duration_since(t) < Duration::from_millis(500))
                        {
                            self.last_esc = None;
                            self.open_tree_selector();
                        }
                        return false;
                    }
                    if self.running.is_some() {
                        self.restore_queued();
                    }
                    self.cancel_run();
                    self.cancel_login();
                    return false;
                }
                if key.code == KeyCode::Up && key.modifiers.contains(KeyModifiers::ALT) {
                    self.restore_queued();
                    return false;
                }
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                if key.code == KeyCode::Char('c') && ctrl {
                    let now = std::time::Instant::now();
                    if self
                        .last_ctrl_c
                        .replace(now)
                        .is_some_and(|t| now.duration_since(t) < Duration::from_millis(500))
                    {
                        return true;
                    }
                    if self.running.is_some() {
                        self.cancel_run();
                    } else if self.login.is_some() {
                        self.cancel_login();
                    } else {
                        self.editor.clear();
                        self.popup = None;
                    }
                    return false;
                }
                if let KeyCode::Char('p' | 'P') = key.code
                    && ctrl
                {
                    self.cycle_model(key.modifiers.contains(KeyModifiers::SHIFT));
                    return false;
                }
                if key.code == KeyCode::Char('l') && ctrl {
                    if !self.request_host_model_modal(true) {
                        self.open_model_selector(true);
                    }
                    return false;
                }
                if key.code == KeyCode::Char('t') && ctrl {
                    self.toggle_thinking();
                    return false;
                }
                if key.code == KeyCode::Char('g') && ctrl {
                    self.external_edit = true;
                    return false;
                }
                if key.code == KeyCode::Tab && self.login.is_none() {
                    // handle_popup_key consumed Tab if a popup was open;
                    // otherwise Tab force-triggers path completion (pi-style).
                    self.open_path_popup();
                    return false;
                }
                if key.code == KeyCode::BackTab {
                    self.cycle_thinking();
                    return false;
                }
                if key.code == KeyCode::Char('o') && ctrl {
                    self.expand_tools = !self.expand_tools;
                    self.pending.push(format!(
                        "{DIM}[tool output {} from here on]{RESET}",
                        if self.expand_tools {
                            "expanded"
                        } else {
                            "collapsed"
                        }
                    ));
                    return false;
                }
                if key.code == KeyCode::Enter
                    && !key.modifiers.contains(KeyModifiers::ALT)
                    && let Some(login) = &self.login
                {
                    let code = self.editor.text();
                    if !code.trim().is_empty() {
                        let _ = login.code_tx.send(code.trim().to_owned());
                        self.editor.clear();
                        self.pending.push(format!("{DIM}[code submitted]{RESET}"));
                    }
                    return false;
                }
                let before = self.editor.text();
                match self.editor.handle_key(key) {
                    EditorAction::Submit(text) => return self.submit(&text, cmd_tx),
                    EditorAction::Quit => return true,
                    EditorAction::Cancel | EditorAction::None => {}
                }
                if self.editor.text() != before {
                    self.refresh_popup();
                }
            }
            Event::Paste(text) => {
                self.editor.insert_paste(&text);
                self.refresh_popup();
            }
            Event::Resize(width, height) => self.term.resize(width, height),
            _ => {}
        }
        false
    }

    fn cancel_run(&mut self) {
        if let Some(cancel) = &self.running {
            cancel.cancel();
            self.status = "cancelling...".to_owned();
        }
    }

    fn cancel_login(&mut self) {
        if let Some(login) = &self.login {
            login.cancel.cancel();
        }
    }

    /// Returns true when the app should quit.
    fn submit(&mut self, text: &str, cmd_tx: &mpsc::UnboundedSender<Cmd>) -> bool {
        let trimmed = text.trim();
        match trimmed {
            "/quit" | "/exit" => return true,
            "/help" => {
                for spec in COMMANDS {
                    let usage = format!("/{} {}", spec.name, spec.args);
                    self.pending
                        .push(format!("{DIM}{:<20}{}{RESET}", usage.trim_end(), spec.desc));
                }
                self.pending.push(format!(
                    "{DIM}!cmd                run bash(cmd) in the REPL; the model sees it next turn{RESET}"
                ));
                self.pending.push(format!(
                    "{DIM}!!cmd               run bash(cmd) without showing the model{RESET}"
                ));
                self.pending.push(format!(
                    "{DIM}keys: enter send (queues mid-run) · shift+enter newline · tab complete · up/down history{RESET}"
                ));
                self.pending.push(format!(
                    "{DIM}      ctrl-y yank · ctrl-_ undo · ctrl-p cycle model · ctrl-l models · shift+tab thinking level{RESET}"
                ));
                self.pending.push(format!(
                    "{DIM}      ctrl-t show/hide thinking · ctrl-o expand tool output · ctrl-g $EDITOR · ctrl-z suspend{RESET}"
                ));
                self.pending
                    .push(format!("{DIM}      esc cancel · ctrl-c ctrl-c quit{RESET}"));
                self.pending.push(format!(
                    "{DIM}      esc esc session tree (empty editor){RESET}"
                ));
            }
            "/new" => {
                let _ = cmd_tx.send(Cmd::Reset);
                match SessionStore::new(&self.cwd) {
                    Ok(store) => self.store = store,
                    Err(e) => self.pending.push(format!("\x1b[31merror:{RESET} {e}")),
                }
                self.pending.push(format!("{DIM}[session reset]{RESET}"));
            }
            "/resume" => self.open_resume_selector(),
            "/tree" => self.open_tree_selector(),
            "/fork" => self.open_fork_selector(),
            "/usage" => {
                if !self.request_host_usage_modal() {
                    self.open_usage_selector();
                }
            }
            _ if trimmed == "/model" || trimmed.starts_with("/model ") => {
                let query = trimmed.strip_prefix("/model").unwrap().trim();
                if query.is_empty() {
                    if !self.request_host_model_modal(true) {
                        self.open_model_selector(true);
                    }
                } else {
                    self.switch_model(query, true);
                }
            }
            _ if trimmed == "/submodel" || trimmed.starts_with("/submodel ") => {
                let query = trimmed.strip_prefix("/submodel").unwrap().trim();
                if query.is_empty() {
                    if !self.request_host_model_modal(false) {
                        self.open_model_selector(false);
                    }
                } else {
                    self.switch_model(query, false);
                }
            }
            _ if trimmed == "/login" || trimmed.starts_with("/login ") => {
                let provider = trimmed.strip_prefix("/login").unwrap().trim().to_owned();
                self.start_login(&provider);
            }
            _ if trimmed == "/logout" || trimmed.starts_with("/logout ") => {
                let provider = trimmed.strip_prefix("/logout").unwrap().trim();
                let provider = if provider.is_empty() {
                    self.root_lm.model().provider
                } else {
                    provider.to_owned()
                };
                match self.storage.logout(&provider) {
                    Ok(()) => self
                        .pending
                        .push(format!("{DIM}[logged out of {provider}]{RESET}")),
                    Err(e) => self.pending.push(format!("\x1b[31merror:{RESET} {e}")),
                }
            }
            _ if trimmed.starts_with('!') && trimmed.len() > 1 => {
                // Shell escapes queue mid-run like prompts so they execute
                // against the REPL in submission order.
                if self.running.is_some() {
                    self.queued.push(trimmed.to_owned());
                } else {
                    self.send_shell(trimmed, cmd_tx);
                }
            }
            prompt => {
                // Plain prompts submitted mid-run queue as follow-ups (slash
                // commands above run immediately).
                if self.running.is_some() {
                    self.queued.push(prompt.to_owned());
                } else {
                    self.send_prompt(prompt, cmd_tx);
                }
            }
        }
        false
    }

    /// `!cmd` runs bash(cmd) in the session REPL and replays it to the model
    /// as a repl turn; `!!cmd` runs the same way but stays out of context.
    fn send_shell(&mut self, escape: &str, cmd_tx: &mpsc::UnboundedSender<Cmd>) {
        let (record, cmd) = match escape.strip_prefix("!!") {
            Some(rest) => (false, rest.trim()),
            None => (true, escape[1..].trim()),
        };
        if cmd.is_empty() {
            return;
        }
        let code = format!("print(bash({}))", python_quote(cmd));
        if record {
            self.begin_block();
            self.push_dot(0, format!("{BOLD}repl{RESET}"));
            self.push_preview(0, &code, self.code_limit());
        } else {
            self.pending.push(String::new());
            self.pending.push(format!("{BOLD}${RESET} {cmd}"));
            self.last_gutter = false;
        }
        let _ = cmd_tx.send(Cmd::Shell { code, record });
    }

    fn send_prompt(&mut self, prompt: &str, cmd_tx: &mpsc::UnboundedSender<Cmd>) {
        self.push_user_message(prompt);
        if let Err(e) = self.store.append(session::Role::User, prompt) {
            self.pending
                .push(format!("{YELLOW}session not saved:{RESET} {e}"));
        }
        // Durable turn boundary: an exchange_start without its exchange_end
        // is how a crash mid-turn stays detectable after restart.
        let _ = self.store.begin_exchange();
        let cancel = CancellationToken::new();
        self.running = Some(cancel.clone());
        self.turn = 0;
        self.stream.clear();
        self.thinking.clear();
        self.status = "thinking...".to_owned();
        let _ = cmd_tx.send(Cmd::Prompt {
            text: prompt.to_owned(),
            cancel,
        });
    }

    fn switch_model(&mut self, query: &str, root: bool) {
        let Some(model) = self.registry.find(query).cloned() else {
            self.pending.push(format!(
                "\x1b[31merror:{RESET} unknown model '{query}' (tab after /model lists models)"
            ));
            return;
        };
        if !self.registry.is_supported(&model) {
            self.pending.push(format!(
                "\x1b[31merror:{RESET} model '{}/{}' is not supported",
                model.provider, model.id
            ));
            return;
        }
        let (target, which) = if root {
            (&self.root_lm, "model")
        } else {
            (&self.sub_lm, "sub model")
        };
        target.set_model(model.clone());
        self.model_label = model_label(&self.root_lm.model(), &self.sub_lm.model());
        self.pending.push(format!(
            "{DIM}[{which} → {}/{}]{RESET}",
            model.provider, model.id
        ));
        self.warn_if_missing_credentials(model.provider);
    }

    /// Credential resolution is async; surface a warning through the login
    /// channel instead of blocking the UI.
    fn warn_if_missing_credentials(&self, provider: String) {
        let storage = self.storage.clone();
        let tx = self.login_tx.clone();
        tokio::spawn(async move {
            if !matches!(storage.resolve(&provider).await, Ok(Some(_))) {
                let _ = tx.send(LoginMsg::Info(format!(
                    "no credentials for '{provider}' — type /login {provider}"
                )));
            }
        });
    }

    fn start_login(&mut self, provider: &str) {
        if self.login.is_some() {
            self.pending
                .push(format!("{DIM}[a login is already in progress]{RESET}"));
            return;
        }
        let name = if provider.is_empty() {
            self.root_lm.model().provider
        } else {
            provider.to_owned()
        };
        let Some(provider) = OAuthProviderId::parse(&name) else {
            self.pending.push(format!(
                "\x1b[31merror:{RESET} unknown provider '{name}' (expected: anthropic, openai)"
            ));
            return;
        };
        let (code_tx, code_rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        self.login = Some(LoginState {
            code_tx,
            cancel: cancel.clone(),
        });
        let storage = self.storage.clone();
        let msg_tx = self.login_tx.clone();
        tokio::spawn(async move {
            let callbacks = TuiLoginCallbacks {
                msg_tx: msg_tx.clone(),
                code_rx: tokio::sync::Mutex::new(code_rx),
            };
            let result = storage
                .login(provider, LoginMethod::Browser, &callbacks, &cancel)
                .await;
            let _ = msg_tx.send(LoginMsg::Done {
                provider: provider.storage_key(),
                result: result.map_err(|e| e.to_string()),
            });
        });
    }

    fn handle_login_msg(&mut self, msg: LoginMsg) {
        match msg {
            LoginMsg::Info(text) => {
                for line in text.lines() {
                    self.pending.push(format!("{DIM}{line}{RESET}"));
                }
            }
            LoginMsg::Done { provider, result } => {
                self.login = None;
                match result {
                    Ok(()) => self
                        .pending
                        .push(format!("{BOLD}logged in to {provider}{RESET}")),
                    Err(e) if e.to_lowercase().contains("cancel") => {
                        self.pending.push(format!("{DIM}[login cancelled]{RESET}"))
                    }
                    Err(e) => self
                        .pending
                        .push(format!("\x1b[31mlogin failed:{RESET} {e}")),
                }
            }
        }
    }

    fn code_limit(&self) -> usize {
        if self.expand_tools {
            usize::MAX
        } else {
            CODE_PREVIEW_LINES
        }
    }

    fn output_limit(&self) -> usize {
        if self.expand_tools {
            usize::MAX
        } else {
            OUTPUT_PREVIEW_LINES
        }
    }

    /// Display columns left of the gutter at `depth` (dot and bar prefixes
    /// are the same width: depth columns + anchor = depth+1 columns).
    fn gutter_text_width(&self, depth: usize) -> usize {
        self.term.width().saturating_sub(2 * (depth + 1)).max(1)
    }

    /// Live/scrollback tool block rows using the same gutter formatting as
    /// finalized action blocks.
    fn tool_lines(&self, depth: usize, title: &str, body: &str, max: usize) -> Vec<String> {
        let mut lines = Vec::new();
        let mut rows = wrap_ansi(title, self.gutter_text_width(depth)).into_iter();
        let first = rows.next().unwrap_or_default();
        lines.push(format!("{}{first}{RESET}", dot_prefix(depth)));
        for row in rows {
            lines.push(format!("{}{row}{RESET}", bar_prefix(depth)));
        }

        let trimmed = body.trim_end();
        if !trimmed.is_empty() {
            let avail = self.gutter_text_width(depth).saturating_sub(2).max(1);
            let rows: Vec<String> = trimmed
                .lines()
                .flat_map(|line| wrap_ansi(line, avail))
                .collect();
            let shown = rows.len().min(max);
            for row in &rows[..shown] {
                lines.push(format!("{}{DIM}  {row}{RESET}", bar_prefix(depth)));
            }
            if rows.len() > max {
                lines.push(format!(
                    "{}{DIM}  … ({} more lines){RESET}",
                    bar_prefix(depth),
                    rows.len() - max
                ));
            }
        }
        lines
    }

    fn live_thinking_lines(&self) -> Vec<String> {
        if self.thinking.trim().is_empty() {
            return Vec::new();
        }
        if self.expand_thinking {
            self.tool_lines(
                0,
                &format!("{DIM}thinking{RESET}"),
                &self.thinking,
                usize::MAX,
            )
        } else {
            vec![format!(
                "{}{DIM}thinking hidden · ctrl+t to show{RESET}",
                dot_prefix(0)
            )]
        }
    }

    fn flush_thinking(&mut self) {
        let thinking = std::mem::take(&mut self.thinking);
        if thinking.trim().is_empty() {
            return;
        }
        if self.expand_thinking {
            self.push_dot(0, format!("{DIM}thinking{RESET}"));
            self.push_preview(0, &thinking, usize::MAX);
        } else {
            self.push_dot(0, format!("{DIM}thinking hidden · ctrl+t to show{RESET}"));
        }
    }

    /// Append an action row to the active block buffer, or straight to
    /// scrollback when no block is in flight.
    fn push_row(&mut self, row: String) {
        if self.block_active {
            self.block.push((row, None));
        } else {
            self.pending.push(row);
        }
    }

    /// Start buffering the depth-0 REPL block that is about to execute. Rows
    /// pushed until `commit_block` render live with the pending background.
    fn begin_block(&mut self) {
        if !self.last_gutter {
            self.pending.push(String::new());
        }
        self.block_active = true;
    }

    /// Commit the buffered block to scrollback over its final status
    /// background, followed by any prose that streamed in behind it.
    fn commit_block(&mut self, bg: &str) {
        if !self.block_active {
            return;
        }
        self.block_active = false;
        self.tool_rows_from = None;
        let width = self.term.width();
        for (row, row_bg) in std::mem::take(&mut self.block) {
            self.pending
                .push(fill_bg(&row, row_bg.unwrap_or(bg), width));
        }
        let after = std::mem::take(&mut self.after_block);
        self.pending.extend(after);
    }

    /// First line of an action block: gutter columns, then a `⏺` anchor.
    /// Wrapped continuations keep the gutter. A depth-0 action that follows
    /// prose gets a separating blank line first.
    fn push_dot(&mut self, depth: usize, text: String) {
        if depth == 0 && !self.last_gutter && !self.block_active {
            self.pending.push(String::new());
        }
        let mut rows = wrap_ansi(&text, self.gutter_text_width(depth)).into_iter();
        let first = rows.next().unwrap_or_default();
        self.push_row(format!("{}{first}{RESET}", dot_prefix(depth)));
        for row in rows {
            self.push_row(format!("{}{row}{RESET}", bar_prefix(depth)));
        }
        self.last_gutter = true;
    }

    /// Continuation/result line inside an action block; wraps to the
    /// terminal width so every row keeps the gutter.
    fn push_bar(&mut self, depth: usize, text: String) {
        for row in wrap_ansi(&text, self.gutter_text_width(depth)) {
            self.push_row(format!("{}{DIM}{row}{RESET}", bar_prefix(depth)));
        }
        self.last_gutter = true;
    }

    /// Indented code/output preview inside an action block: at most `max`
    /// visual rows (counting wraps), then a `… (n more lines)` marker.
    fn push_preview(&mut self, depth: usize, text: &str, max: usize) {
        let trimmed = text.trim_end();
        if trimmed.is_empty() {
            return;
        }
        let avail = self.gutter_text_width(depth).saturating_sub(2).max(1);
        let rows: Vec<String> = trimmed
            .lines()
            .flat_map(|line| wrap_ansi(line, avail))
            .collect();
        let shown = rows.len().min(max);
        for row in &rows[..shown] {
            self.push_bar(depth, format!("  {row}"));
        }
        if rows.len() > max {
            self.push_bar(depth, format!("  … ({} more lines)", rows.len() - max));
        }
    }

    /// Echo a user message into scrollback as a pi-style container: full-width
    /// `userMessageBg` fill with one cell of padding on every side.
    fn push_user_message(&mut self, text: &str) {
        let width = self.term.width();
        let inner = width.saturating_sub(2).max(1);
        self.pending.push(String::new());
        self.pending.push(fill_bg("", USER_BG, width));
        for line in text.lines() {
            for row in wrap_ansi(line, inner) {
                self.pending
                    .push(fill_bg(&format!(" {row}"), USER_BG, width));
            }
        }
        self.pending.push(fill_bg("", USER_BG, width));
        self.last_gutter = false;
    }

    /// Render finalized prose (markdown) into scrollback. While a block is
    /// in flight the prose waits behind it so scrollback stays chronological.
    fn push_prose(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let rendered = render_markdown(text, self.markdown_width());
        let sink = if self.block_active {
            &mut self.after_block
        } else {
            &mut self.pending
        };
        sink.push(String::new());
        sink.extend(rendered);
        self.last_gutter = false;
    }

    fn handle_rlm_event(&mut self, event: RlmEvent) {
        match event {
            RlmEvent::IterationStart {
                depth: 0,
                iteration,
            } => {
                self.turn = iteration + 1;
                self.status = "thinking...".to_owned();
                self.stream.clear();
                self.thinking.clear();
                self.streamed = false;
            }
            RlmEvent::RootDelta {
                depth: 0, delta, ..
            } => {
                self.stream.push_str(&delta);
                self.streamed = true;
                // Flush completed paragraphs into scrollback as they stream;
                // only the unfinished tail stays live.
                let stable = stable_prefix(&self.stream);
                if stable > 0 {
                    let rest = self.stream.split_off(stable);
                    let para = std::mem::replace(&mut self.stream, rest);
                    self.push_prose(&para);
                }
            }
            RlmEvent::RootThinkingDelta {
                depth: 0, delta, ..
            } => {
                self.thinking.push_str(&delta);
            }
            RlmEvent::RootText { depth: 0, text, .. } => {
                self.flush_thinking();
                // Streaming: paragraphs and blocks were flushed as they
                // arrived; render the leftover tail. Non-streaming: render
                // the full text's prose (blocks were previewed by
                // `CodeExecStart`).
                let remaining = if self.streamed {
                    std::mem::take(&mut self.stream)
                } else {
                    text
                };
                for segment in split_prose(&remaining) {
                    self.push_prose(segment);
                }
                self.stream.clear();
                self.streamed = false;
            }
            RlmEvent::CodeExecStart { depth, code, .. } => {
                if depth == 0 {
                    // The fence just closed mid-stream: flush the prose before
                    // it and consume both from the live tail.
                    if let Some((prose, rest)) = split_at_first_block(&self.stream) {
                        let (prose, rest) = (prose.to_owned(), rest.to_owned());
                        self.push_prose(&prose);
                        self.stream = rest;
                    }
                    self.status = "running code...".to_owned();
                    self.begin_block();
                    self.live_code = Some(code.clone());
                }
                self.push_dot(depth, format!("{BOLD}repl{RESET}"));
                self.push_preview(depth, &code, self.code_limit());
            }
            RlmEvent::CodeExecEnd {
                depth,
                stdout,
                stderr,
                duration,
                ..
            } => {
                self.push_bar(depth, format!("exec {:.1}s", duration.as_secs_f64()));
                self.push_preview(depth, &stdout, self.output_limit());
                self.push_preview(depth, &stderr, self.output_limit());
                if depth == 0 {
                    let bg = if stderr.trim().is_empty() {
                        REPL_BG
                    } else {
                        REPL_ERROR_BG
                    };
                    self.commit_block(bg);
                    self.status = "thinking...".to_owned();
                    // Checkpoint the completed step as it happens (not at
                    // exchange end): a crash later in the turn must not lose
                    // work already paid for. `branch_restore` replays these
                    // to rebuild the REPL env.
                    if let Some(code) = self.live_code.take() {
                        let _ = self.store.append_repl(
                            &code,
                            &stdout,
                            &stderr,
                            duration.as_secs_f64(),
                            session::ReplKind::Agent,
                            std::mem::take(&mut self.live_tools),
                            std::mem::take(&mut self.live_subcalls),
                        );
                    }
                    self.open_tool_args.clear();
                    self.open_subcall_previews.clear();
                }
            }
            // Prelude tools run inside the REPL block at `depth`, so they
            // render one gutter level deeper, nested in that block.
            RlmEvent::ToolStart {
                depth,
                call_id,
                name,
                args,
            } => {
                if depth == 0 {
                    self.open_tool_args.insert(call_id, args.clone());
                }
                if self.block_active {
                    self.tool_rows_from = Some(self.block.len());
                }
                self.push_dot(depth + 1, tool_title(&name, &args));
                if depth == 0 {
                    self.status = format!("{name}...");
                }
            }
            RlmEvent::ToolEnd {
                depth,
                call_id,
                name,
                result,
                error,
                duration,
            } => {
                if depth == 0 {
                    let args = self.open_tool_args.remove(&call_id).unwrap_or_default();
                    // Replay only ever renders a short preview, and the full
                    // text already lands in the repl stdout payload, so cap
                    // what gets persisted per tool call.
                    self.live_tools.push(session::ToolCall {
                        name: name.clone(),
                        args,
                        result: truncate_for_log(&result),
                        error: error.as_deref().map(truncate_for_log),
                        duration_secs: duration.as_secs_f64(),
                    });
                }
                let bg = if error.is_some() {
                    TOOL_ERROR_BG
                } else {
                    TOOL_SUCCESS_BG
                };
                match error {
                    Some(error) => {
                        self.push_bar(depth + 1, format!("failed {:.1}s", duration.as_secs_f64()));
                        self.push_preview(depth + 1, &error, self.output_limit());
                    }
                    None => {
                        self.push_bar(depth + 1, format!("done {:.1}s", duration.as_secs_f64()));
                        self.push_preview(depth + 1, &result, self.output_limit());
                    }
                }
                if let Some(start) = self.tool_rows_from.take() {
                    for (_, row_bg) in &mut self.block[start..] {
                        *row_bg = Some(bg);
                    }
                }
                if depth == 0 {
                    self.status = "running code...".to_owned();
                }
            }
            RlmEvent::SubcallStart {
                depth,
                model,
                prompt_preview,
            } => {
                if depth == 0 {
                    self.open_subcall_previews
                        .push_back(prompt_preview.clone());
                }
                self.push_dot(
                    depth,
                    format!("{BOLD}{model}{RESET} ← \"{prompt_preview}\""),
                );
                if depth == 0 {
                    self.status = format!("waiting on {model}...");
                }
            }
            RlmEvent::SubcallEnd {
                depth,
                model,
                duration,
                error,
            } => {
                if depth == 0 {
                    let prompt_preview = self.open_subcall_previews.pop_front().unwrap_or_default();
                    self.live_subcalls.push(session::SubcallRecord {
                        model: model.clone(),
                        prompt_preview,
                        error: error.clone(),
                        duration_secs: duration.as_secs_f64(),
                    });
                }
                let text = match error {
                    Some(error) => format!("{model} failed: {error}"),
                    None => format!("{model} done {:.1}s", duration.as_secs_f64()),
                };
                self.push_bar(depth, text);
            }
            RlmEvent::RunStart { depth } if depth > 0 => {
                self.push_dot(depth, format!("{BOLD}rlm{RESET}"));
            }
            RlmEvent::RunEnd {
                depth,
                stop,
                iterations,
                duration,
            } if depth > 0 => {
                self.push_bar(
                    depth,
                    format!(
                        "{} · {} turn{} · {:.1}s",
                        format!("{stop:?}").to_lowercase(),
                        iterations,
                        if iterations == 1 { "" } else { "s" },
                        duration.as_secs_f64()
                    ),
                );
            }
            RlmEvent::LimitHit {
                depth,
                limit,
                detail,
            } => {
                self.push_bar(
                    depth,
                    format!("limit {} · {detail}", format!("{limit:?}").to_lowercase()),
                );
            }
            _ => {}
        }
    }

    fn handle_done(&mut self, done: Done, cmd_tx: &mpsc::UnboundedSender<Cmd>) {
        match done {
            Done::Run(completion) => self.handle_run_done(completion),
            Done::Shell { code, record, result } => self.handle_shell_done(code, record, result),
            Done::Restored(result) => {
                if let Err(e) = result {
                    self.pending
                        .push(format!("\x1b[31mrestore failed:{RESET} {e}"));
                }
            }
        }
        self.drain_queue(cmd_tx);
    }

    /// Send the next queued submission once nothing is in flight.
    fn drain_queue(&mut self, cmd_tx: &mpsc::UnboundedSender<Cmd>) {
        if self.running.is_some() || self.queued.is_empty() {
            return;
        }
        let next = self.queued.remove(0);
        if next.starts_with('!') {
            self.send_shell(&next, cmd_tx);
        } else {
            self.send_prompt(&next, cmd_tx);
        }
    }

    fn handle_shell_done(&mut self, code: String, record: bool, result: Result<ReplResult, String>) {
        match result {
            Ok(res) if record => {
                self.push_bar(0, format!("exec {:.1}s", res.duration.as_secs_f64()));
                self.push_preview(0, &res.stdout, self.output_limit());
                self.push_preview(0, &res.stderr, self.output_limit());
                let bg = if res.stderr.trim().is_empty() {
                    REPL_BG
                } else {
                    REPL_ERROR_BG
                };
                self.commit_block(bg);
                let _ = self.store.append_repl(
                    &code,
                    &res.stdout,
                    &res.stderr,
                    res.duration.as_secs_f64(),
                    session::ReplKind::Shell,
                    Vec::new(),
                    Vec::new(),
                );
            }
            Ok(res) => {
                let output = format!("{}{}", res.stdout, res.stderr);
                if output.trim().is_empty() {
                    self.pending.push(format!("{DIM}(no output){RESET}"));
                } else {
                    self.pending
                        .extend(output.trim_end().lines().map(str::to_owned));
                }
                self.last_gutter = false;
            }
            Err(e) => {
                if self.block_active {
                    self.push_bar(0, format!("failed: {e}"));
                    self.commit_block(REPL_ERROR_BG);
                } else {
                    self.pending.push(format!("\x1b[31merror:{RESET} {e}"));
                    self.last_gutter = false;
                }
            }
        }
    }

    fn handle_run_done(&mut self, completion: RlmCompletion) {
        // A block still open here never finished (cancel/failure mid-exec).
        self.commit_block(REPL_ERROR_BG);
        self.live_code = None;
        self.running = None;
        self.stream.clear();
        self.thinking.clear();
        self.pending.push(String::new());
        if completion.stop == StopCause::Failed {
            let error = completion.error.unwrap_or_else(|| "run failed".to_owned());
            self.pending.push(format!("\x1b[31merror:{RESET} {error}"));
        } else if !completion.response.is_empty() {
            self.push_assistant_message(&completion.response);
        }
        // Mirror the env's history condition so the store and the REPL's
        // history_N variables stay aligned.
        if completion.stop != StopCause::Failed && !completion.response.trim().is_empty() {
            let usage = session::SessionUsage {
                input: completion.usage.input,
                output: completion.usage.output,
                cache_read: completion.usage.cache_read,
                cache_write: completion.usage.cache_write,
                cost_total: completion.usage.cost.total,
                model: self.model_label.clone(),
            };
            let _ = self.store.append_with_usage(
                session::Role::Assistant,
                &completion.response,
                Some(usage),
            );
        }
        // Durably close the turn: completed, failed, or interrupted is a
        // recorded fact, not an inference from a missing assistant message.
        let status = match completion.stop {
            StopCause::Failed => session::ExchangeStatus::Failed,
            StopCause::Cancelled => session::ExchangeStatus::Interrupted,
            _ => session::ExchangeStatus::Completed,
        };
        let _ = self.store.end_exchange(status);
        self.usage = completion.usage;
        self.pending.push(String::new());
        self.pending.push(format!(
            "{DIM}{} · {} turn{} · {:.1}s · {} tok · ${:.4}{RESET}",
            format!("{:?}", completion.stop).to_lowercase(),
            completion.iterations,
            if completion.iterations == 1 { "" } else { "s" },
            completion.duration.as_secs_f64(),
            format_tokens(self.usage.total_tokens),
            self.usage.cost.total
        ));
        self.last_gutter = false;
    }

    fn markdown_width(&self) -> usize {
        self.term.width().saturating_sub(2).min(100)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn models() -> Vec<String> {
        vec![
            "anthropic/claude-opus-4-7".to_owned(),
            "openai/gpt-5.2".to_owned(),
        ]
    }

    #[test]
    fn stable_prefix_flushes_completed_paragraphs() {
        // No completed blank line yet: nothing stable.
        assert_eq!(stable_prefix("hello wor"), 0);
        assert_eq!(stable_prefix("hello\n"), 0);
        // A finished blank line marks the paragraph as stable.
        assert_eq!(stable_prefix("para one\n\npara two"), 10);
        // The trailing blank line itself must be complete.
        let two = "a\n\nb\n\n";
        assert_eq!(stable_prefix(two), two.len());
    }

    #[test]
    fn stable_prefix_respects_fences_and_repl_blocks() {
        // Blank lines inside an open fence are not boundaries.
        assert_eq!(stable_prefix("```python\nx = 1\n\ny = 2\n"), 0);
        let closed = "```python\nx = 1\n```\n\ntail";
        assert_eq!(stable_prefix(closed), closed.len() - "tail".len());
        // Never flush past the start of a complete ```repl block: it is
        // consumed from the stream by CodeExecStart.
        let with_block = "intro\n\n```repl\nprint(1)\n```\n\nafter";
        assert_eq!(stable_prefix(with_block), "intro\n\n".len());
        // The engine's regex matches mid-line fences, so blank lines inside
        // a still-open block glued to prose are not boundaries either.
        let mid_line = "para\n\nintro.```repl\nx = 1\n\ny = 2\n";
        assert_eq!(stable_prefix(mid_line), "para\n\n".len());
        // A trailing partial fence opening is held back too.
        assert_eq!(stable_prefix("para\n\n```re"), "para\n\n".len());
    }

    #[test]
    fn first_block_start_guards_repl_fences() {
        // Complete, unclosed, and partial fences all stop the prefix.
        assert_eq!(first_block_start("a\n```repl\nx\n```\nb"), 2);
        assert_eq!(first_block_start("a.```repl\nx = 1"), 2);
        assert_eq!(first_block_start("text ```re"), 5);
        // Ordinary fences and inline code are not held back.
        let python = "```python\nx\n```\ndone";
        assert_eq!(first_block_start(python), python.len());
        let inline = "use `foo` here";
        assert_eq!(first_block_start(inline), inline.len());
        // A bare trailing ``` could still grow into ```repl: held back.
        assert_eq!(first_block_start("done\n```"), "done\n".len());
    }

    #[test]
    fn live_block_body_streams_open_and_closed_repl_blocks() {
        assert_eq!(live_block_body("text ```re"), None);
        assert_eq!(live_block_body("text ```repl"), None);
        assert_eq!(live_block_body("text ```repl \t"), None);
        assert_eq!(live_block_body("text ```python\nx = 1"), None);

        assert_eq!(live_block_body("text ```repl\n"), Some(""));
        assert_eq!(live_block_body("text ```repl\nprint(1)"), Some("print(1)"));
        assert_eq!(
            live_block_body("text ```repl\nprint(1)\n```\nafter"),
            Some("print(1)")
        );
        assert_eq!(
            live_block_body("text\n```repl \t\nanswer = 42\nmore"),
            Some("answer = 42\nmore")
        );
    }

    #[test]
    fn python_quote_escapes_specials() {
        assert_eq!(python_quote("ls -la"), "\"ls -la\"");
        assert_eq!(
            python_quote("echo \"a\\b\"\nls\t."),
            "\"echo \\\"a\\\\b\\\"\\nls\\t.\""
        );
    }

    #[test]
    fn slash_lists_all_commands() {
        let popup = complete("/", &models()).unwrap();
        assert_eq!(popup.items.len(), COMMANDS.len());
        assert!(popup.kind == PopupKind::Command);
        assert_eq!(popup.prefix, "/");
    }

    #[test]
    fn command_prefix_filters() {
        // Fuzzy: "/mo" matches "model" (best) and "submodel".
        let popup = complete("/mo", &models()).unwrap();
        assert_eq!(popup.items.len(), 2);
        assert_eq!(popup.items[0].value, "model");
        assert_eq!(popup.items[1].value, "submodel");
        assert!(complete("/zzz", &models()).is_none());
        assert!(complete("hello", &models()).is_none());
    }

    #[test]
    fn model_argument_completion() {
        let popup = complete("/model ", &models()).unwrap();
        assert_eq!(popup.items.len(), 2);
        assert!(popup.kind == PopupKind::Argument);
        let popup = complete("/model opus", &models()).unwrap();
        assert_eq!(popup.items.len(), 1);
        assert_eq!(popup.items[0].value, "anthropic/claude-opus-4-7");
        assert_eq!(popup.prefix, "opus");
    }

    #[test]
    fn provider_argument_completion() {
        let popup = complete("/login a", &models()).unwrap();
        assert_eq!(popup.items.len(), 2); // both contain 'a'
        let popup = complete("/login anth", &models()).unwrap();
        assert_eq!(popup.items[0].value, "anthropic");
    }

    #[test]
    fn no_argument_completion_for_other_commands() {
        assert!(complete("/new ", &models()).is_none());
        assert!(complete("/model two words", &models()).is_none());
    }
}

