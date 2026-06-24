//! in-TUI via /login, mirroring ref/phi: the OAuth flow runs in the
//! background, the URL lands in scrollback, and the editor doubles as the
//! paste-the-redirect-URL input (racing the localhost callback server).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use phi_runtime::AppRuntime;

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
    Editor, EditorAction, LiveCursor, RawModeGuard, Selector, SelectorItem,
    SelectorOutcome, Spinner, Terminal, detect_terminal_colors, fuzzy_filter, highlight_code,
    highlight_diff_line, highlight_output, render_markdown, visible_width, wrap_ansi,
    wrap_ansi_locate,
};

use phi_session::files::{self, FileSearcher};
use phi_session::session::{self, SessionStore};
use phi_session::transcript::{BlockSpec, Transcript};
use phi_session::usage::load_usage_report;
use phi_session::usage::usage_overlay_lines;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

pub(super) const RESET: &str = "\x1b[0m";
pub(super) const BOLD: &str = "\x1b[1m";
pub(super) const DIM: &str = "\x1b[2m";
pub(super) const CYAN: &str = "\x1b[36m";
/// Truecolor ANSI escapes for the inline TUI's transcript/chrome rendering,
/// derived from the host terminal's reported colors when available. Mirrors
/// opencode's `generateSystem` mapping. `default_const()` returns the previous
/// hardcoded values as the fallback. Basic ANSI colors (`RESET`, `BOLD`, etc.)
/// stay as module-level `&str` since they already reference the terminal palette.
mod palette;
mod render;
mod tool_display;
mod popup;

pub use palette::InlinePalette;
pub(crate) use render::{OpenToolRow, OpenSubcallRow, TranscriptBlock};
pub(crate) use tool_display::{format_model_pricing, format_tokens, tool_identity, tool_title};

use palette::*;
use render::*;
use tool_display::*;
use popup::*;

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
    pub runtime: Arc<AppRuntime>,
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

pub async fn run(setup: TuiSetup, mut io: SessionIo) -> Result<()> {
    let TuiSetup {
        rlm,
        payload,
        mut events,
        missing_credentials,
        runtime,
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
                    if done_tx
                        .send(Done::Shell {
                            code,
                            record,
                            result,
                        })
                        .is_err()
                    {
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
        runtime,
    );
    // Probe the host terminal's colors (OSC 10/11/4) once, inside raw mode,
    // and derive the inline TUI palette from them. Falls back to the built-in
    // hardcoded palette when the terminal doesn't answer in time.
    if !io.embedded
        && let Some(colors) = detect_terminal_colors(Duration::from_millis(150))
    {
        ui.palette = InlinePalette::from_terminal_colors(&colors);
    }
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
                Some(Ok(event)) => ui.handle_input(event, &cmd_tx).await,
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
                ui.refresh_open_activity_rows();
                for msg in ui.runtime.drain_notifications() {
                    ui.pending.push(msg);
                }
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
                Err(e) => ui.pending.push(ui.err(&e)),
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
                Err(e) => ui.pending.push(ui.err(&e)),
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
    let _ = ui.runtime.shutdown_session().await;
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
pub fn external_editor(text: &str) -> Result<Option<String>, String> {
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
    palette: InlinePalette,
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
    /// ctrl+o: show full code/output instead of previews across the
    /// transcript by rebuilding retained history, pi-style.
    expand_tools: bool,
    /// ctrl+t: show provider thinking across the transcript by rebuilding
    /// retained history, pi-style.
    expand_thinking: bool,
    /// Follow-up prompts queued while a turn is in flight; sent FIFO as
    /// turns complete.
    queued: Vec<String>,
    /// Time of the last ctrl-c; a second one within 500ms quits.
    last_ctrl_c: Option<std::time::Instant>,
    /// Lines staged by event handlers since the last draw; each draw folds
    /// them into a retained transcript block before rendering.
    pending: Vec<String>,
    /// Retained transcript renderer/commit seam, restored from the archived
    /// Phi transcript-block model.
    transcript: Transcript,
    /// Finalized transcript blocks retained across frames.
    transcript_blocks: Vec<TranscriptBlock>,
    /// Monotonic key source for finalized transcript blocks.
    next_transcript_block_id: u64,
    /// Rows of the in-flight depth-0 ```repl``` block (with everything nested in
    /// it), shown live and then committed to scrollback verbatim.
    block: Vec<String>,
    block_active: bool,
    /// Code of the in-flight depth-0 agent ```repl``` block, stashed at
    /// `CodeExecStart` and persisted to the session store at `CodeExecEnd`.
    live_code: Option<String>,
    /// Tool calls observed inside the in-flight depth-0 ```repl``` block; args
    /// stashed at `ToolStart`, completed at `ToolEnd`, persisted with the
    /// block at `CodeExecEnd` so resume re-renders the same tool cards.
    live_tools: Vec<session::ToolCall>,
    /// Args of tools still executing inside the buffered depth-0 REPL block,
    /// keyed by (RLM event depth, engine call_id). Child REPLs have their own
    /// call-id spaces, so depth is part of the key.
    open_tool_args: std::collections::HashMap<(usize, u64), serde_json::Value>,
    /// Live tool status rows inside the buffered block.
    open_tool_rows: std::collections::HashMap<(usize, u64), OpenToolRow>,
    /// Sub-LLM calls observed inside the in-flight depth-0 ```repl``` block.
    live_subcalls: Vec<session::SubcallRecord>,
    /// Live subcall status rows inside the buffered block.
    open_subcall_rows: std::collections::VecDeque<OpenSubcallRow>,

    /// Prose flushed while a block is active; committed after the block so
    /// scrollback stays chronological.
    after_block: Vec<String>,
    /// Whether the last scrollback line was nested activity; a blank line is
    /// inserted before a depth-0 action that follows prose.
    last_activity: bool,
    /// Streamed root-model text for the current iteration, minus the
    /// paragraphs/fences already flushed to scrollback.
    stream: String,
    /// Streamed provider thinking for the current iteration.
    thinking: String,
    /// Provider thinking captured across the whole in-flight turn so replay
    /// can re-render historical thinking blocks under the current toggle.
    thinking_log: String,
    /// Whether any `RootDelta` arrived this iteration; when false the model
    /// did not stream and `RootText` carries the only copy of the text.
    streamed: bool,
    status: String,
    turn: usize,
    model_label: String,
    storage: Arc<AuthStorage>,
    registry: Arc<Registry>,
    root_lm: Arc<ProviderLm>,
    sub_lm: Arc<ProviderLm>,
    runtime: Arc<AppRuntime>,
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
        runtime: Arc<AppRuntime>,
    ) -> Self {
        let UiSetup {
            storage,
            registry,
            root_lm,
            sub_lm,
            store,
            ..
        } = setup;
        let model_names = registry
            .supported_models()
            .map(|m| format!("{}/{}", m.provider, m.id))
            .collect();
        Self {
            term,
            editor: Editor::new(),
            spinner: Spinner::new(),
            palette: InlinePalette::default_const(),
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
            transcript: Transcript::new(),
            transcript_blocks: Vec::new(),
            next_transcript_block_id: 0,
            block: Vec::new(),
            block_active: false,
            live_code: None,
            live_tools: Vec::new(),
            open_tool_args: std::collections::HashMap::new(),
            open_tool_rows: std::collections::HashMap::new(),
            live_subcalls: Vec::new(),
            open_subcall_rows: std::collections::VecDeque::new(),
            after_block: Vec::new(),
            last_activity: false,
            stream: String::new(),
            thinking: String::new(),
            thinking_log: String::new(),
            streamed: false,
            status: String::new(),
            turn: 0,
            model_label: model_label(&root_lm.model(), &sub_lm.model()),
            storage,
            registry,
            root_lm,
            sub_lm,
            runtime,
            model_names,
            login_tx,
            host_modal_tx,
            pending_host_modal: None,
            usage: Usage::default(),
            branch: None,
            branch_at: std::time::Instant::now() - Duration::from_secs(60),
        }
    }

    fn err(&self, msg: &impl std::fmt::Display) -> String {
        format!("{}error:{RESET} {msg}", &self.palette.error_fg)
    }

    fn warn(&self, msg: &impl std::fmt::Display) -> String {
        format!("{}{msg}{RESET}", &self.palette.warning_fg)
    }

    fn greet(&mut self, missing_credentials: &[String]) {
        self.pending.push(format!(
            "{BOLD}phi{RESET} {DIM}· {} · /help for commands{RESET}",
            self.model_label
        ));
        for provider in missing_credentials {
            self.pending.push(format!(
                "{}no credentials for '{provider}'{RESET} {DIM}— type /login to authenticate{RESET}",
                &self.palette.warning_fg
            ));
        }
    }

    fn draw(&mut self) -> std::io::Result<()> {
        self.flush_pending_to_transcript();

        let (chrome, chrome_cursor) = self.chrome_lines();
        let transcript_height = self.term.height().saturating_sub(chrome.len()).max(1);
        let live_blocks = self.current_live_transcript_blocks();

        let mut specs = Vec::with_capacity(self.transcript_blocks.len() + live_blocks.len());
        for block in &self.transcript_blocks {
            specs.push(BlockSpec {
                key: &block.key,
                lines: &block.lines,
                finalized: true,
                max_height: None,
            });
        }
        for block in &live_blocks {
            specs.push(BlockSpec {
                key: &block.key,
                lines: &block.lines,
                finalized: block.finalized,
                max_height: block.max_height,
            });
        }

        let frame = self
            .transcript
            .render(transcript_height, &specs);

        let transcript_live_len = frame.live.len();
        let mut live = frame.live;
        live.extend(chrome);
        let cursor = chrome_cursor.map(|mut cursor| {
            cursor.line += transcript_live_len;
            cursor
        });
        self.term.render(&frame.scrollback, &live, cursor)
    }

    fn flush_pending_to_transcript(&mut self) {
        let pending = std::mem::take(&mut self.pending);
        if pending.is_empty() {
            return;
        }
        self.transcript_blocks.push(TranscriptBlock {
            key: format!("tx:{}", self.next_transcript_block_id),
            lines: pending,
            finalized: true,
            max_height: None,
        });
        self.next_transcript_block_id += 1;
    }

    fn clear_transcript(&mut self) {
        self.transcript.reset();
        self.transcript_blocks.clear();
        self.next_transcript_block_id = 0;
        self.pending.clear();
        self.block.clear();
        self.after_block.clear();
        self.last_activity = false;
    }

    fn current_live_transcript_blocks(&self) -> Vec<TranscriptBlock> {
        let mut blocks = Vec::new();

        if self.running.is_some() {
            let thinking = self.live_thinking_lines();
            if !thinking.is_empty() {
                blocks.push(TranscriptBlock {
                    key: "live:thinking".to_owned(),
                    lines: thinking,
                    finalized: false,
                    max_height: Some(LIVE_THINKING_MAX_HEIGHT),
                });
            }

            if self.block_active {
                blocks.push(TranscriptBlock {
                    key: "live:repl-block".to_owned(),
                    lines: render_repl_block(&self.block, Some(self.turn)),
                    finalized: false,
                    max_height: Some(CODE_PREVIEW_LINES),
                });

                if !self.after_block.is_empty() {
                    blocks.push(TranscriptBlock {
                        key: "live:after-block".to_owned(),
                        lines: self.after_block.clone(),
                        finalized: true,
                        max_height: Some(OUTPUT_PREVIEW_LINES),
                    });
                }
            }

            let visible = self.stream[..first_block_start(&self.stream)].trim();
            if !visible.is_empty() {
                blocks.push(TranscriptBlock {
                    key: "live:stream-prose".to_owned(),
                    lines: self.assistant_card_lines(visible),
                    finalized: false,
                    max_height: Some(LIVE_PROSE_MAX_HEIGHT),
                });
            }

            if let Some(block) = live_block_body(&self.stream) {
                blocks.push(TranscriptBlock {
                    key: "live:preview-repl".to_owned(),
                    lines: render_repl_block(
                        &self.repl_code_lines(0, block, self.code_limit()),
                        None,
                    ),
                    finalized: false,
                    max_height: Some(CODE_PREVIEW_LINES),
                });
            }
        }

        blocks
    }


    /// Agent + usage segment for the chrome header: " PHI  1.2k  $0.01".
    fn render_chrome_agent(&self) -> String {
        let active = self.usage.total_tokens > 0;
        let mut result = String::with_capacity(64);

        let agent_str = format!("{} PHI", agent_symbol());
        result.push_str(&format!("{}{agent_str}{RESET}", &self.palette.chrome_agent_fg));

        if active {
            let tokens = format_tokens(self.usage.total_tokens);
            result.push_str(&format!(" {}{tokens}{RESET}", &self.palette.chrome_agent_fg));
        }

        if active && self.usage.cost.total > 0.0 {
            result.push_str(&format!(
                " {}{}{:.2}{RESET}",
                &self.palette.chrome_cost_fg,
                cost_symbol(),
                self.usage.cost.total
            ));
        }

        result
    }

    /// Model + effort segment for the chrome header: "  glm-5.2 XHIGH".
    fn render_chrome_model(&self) -> String {
        let mut result = String::with_capacity(64);

        let model_id = &self.root_lm.model().id;
        let short_model = model_id.rsplit('/').next().unwrap_or(model_id);
        result.push_str(&format!(
            "{}{} {short_model}{RESET}",
            &self.palette.chrome_model_fg,
            model_symbol()
        ));

        if let Some(effort) = effort_label(self.root_lm.thinking(), self.term.width()) {
            let thinking_fg = thinking_level_color(self.root_lm.thinking(), &self.palette);
            result.push_str(&format!(" {}{effort}{RESET}", thinking_fg));
        }

        result
    }

    fn chrome_lines(&mut self) -> (Vec<String>, Option<LiveCursor>) {
        let mut lines: Vec<String> = Vec::new();
        let width = self.term.width().max(12);
        let p = &self.palette;

        if self.running.is_some() {
            lines.push(format!(
                "{CYAN}{}{RESET} {DIM}{} · turn {}{RESET}",
                self.spinner.frame(),
                self.status,
                self.turn
            ));
        }

        if self.branch_at.elapsed() > Duration::from_secs(5) {
            self.branch = git_branch(&self.cwd);
            self.branch_at = std::time::Instant::now();
        }
        let pwd = display_path(&self.cwd);

        let framed = self.selector.is_some();
        if framed {
            let header_plain = "φ";
            let header_fill = width
                .saturating_sub(header_plain.chars().count() + 5)
                .max(1);
            lines.push(format!(
                "{chrome_border}┌─ {BOLD}φ{RESET} {}┐{RESET}",
                "─".repeat(header_fill),
                chrome_border = &p.chrome_border,
            ));
        }

        let cursor;
        if let Some((_, widget)) = &self.selector {
            let avail = self.term.height().saturating_sub(lines.len() + 2).max(1);
            let (sel_lines, sel_cursor) = widget.render(width, avail);
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
            lines.push(format!("{muted_fg}{hint}{RESET}", muted_fg = &p.muted_fg));
        } else {
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

            // Separator line with the same bg inversion as phi-mux's statusbar.
            let sep_width = width;
            let sep = "\u{2500}".repeat(sep_width);
            lines.push(format!("{}{sep}{RESET}", &p.statusbar_bg));

            // Forgecode-style header: all left-aligned.
            //   PHI  dir  branch  model  effort
            let current_dir = self
                .cwd
                .file_name()
                .and_then(|name| name.to_str())
                .map(String::from)
                .unwrap_or_else(|| ".".to_string());
            let mut header = self.render_chrome_agent();
            header.push_str(&format!(
                "  {}{} {current_dir}{RESET}",
                &self.palette.chrome_dir_fg,
                dir_symbol()
            ));
            if let Some(branch) = &self.branch
                && branch != &current_dir
            {
                header.push_str(&format!(
                    "  {}{} {branch}{RESET}",
                    &self.palette.chrome_dir_fg,
                    branch_symbol()
                ));
            }
            header.push_str(&format!("  {}", self.render_chrome_model()));
            lines.push(header);

            // Editor prompt: chevron on row 0, 2-space indent on continuation.
            let editor_top = lines.len();
            let input_inner = width.saturating_sub(2).max(1);
            let mut input_cursor = None;
            let mut rendered_rows_before_cursor = 0usize;
            for (i, line) in editor_lines[self.editor_scroll..end].iter().enumerate() {
                let absolute_row = i + self.editor_scroll;
                let content = if absolute_row == 0 {
                    if line.is_empty() {
                        format!("{}{}{RESET} ", &self.palette.chrome_dir_fg, success_symbol())
                    } else {
                        format!("{}{}{RESET} {line}", &self.palette.chrome_dir_fg, success_symbol())
                    }
                } else {
                    format!("  {line}")
                };
                let avail_inner = if absolute_row == 0 { input_inner } else { width };
                if absolute_row == row {
                    let source_col = if absolute_row == 0 {
                        if line.is_empty() { 2 } else { 2 + col }
                    } else {
                        2 + col
                    };
                    let (wrapped, (cursor_row, cursor_col)) =
                        wrap_ansi_locate(&content, avail_inner, source_col);
                    input_cursor = Some(LiveCursor {
                        line: editor_top + rendered_rows_before_cursor + cursor_row,
                        col: cursor_col,
                    });
                    rendered_rows_before_cursor += wrapped.len();
                    for r in wrapped {
                        lines.push(r);
                    }
                } else {
                    let wrapped = wrap_ansi(&content, avail_inner);
                    rendered_rows_before_cursor += wrapped.len();
                    for r in wrapped {
                        lines.push(r);
                    }
                }
            }
            if end < editor_lines.len() {
                let below = editor_lines.len() - end;
                lines.push(format!("{muted_fg}  \u{2193} {below} more{RESET}", muted_fg = &p.muted_fg));
            }
            cursor = input_cursor;

            if let Some(text) = self.queued.first() {
                let first = text.lines().next().unwrap_or("");
                let more = if text.lines().count() > 1 { " \u{2026}" } else { "" };
                let count = if self.queued.len() > 1 {
                    format!(" \u{00d7}{}", self.queued.len())
                } else {
                    String::new()
                };
                lines.push(format!("{muted_fg}queued{count} > {first}{more}{RESET}", muted_fg = &p.muted_fg));
            }

            if let Some(popup) = &self.popup {
                lines.extend(render_popup(popup));
            }

            let hints: &[&str] = if self.login.is_some() {
                &[
                    "finish login in the browser, or paste the redirect URL and press enter \u{00b7} esc cancels",
                ]
            } else if self.popup.is_some() {
                &["tab complete \u{00b7} up/down select \u{00b7} enter accept \u{00b7} esc dismiss"]
            } else if self.running.is_some() {
                &["enter queue follow-up", "alt+up edit queued \u{00b7} esc cancel"]
            } else {
                &[]
            };
            for hint in hints {
                lines.push(format!("{muted_fg}{hint}{RESET}", muted_fg = &p.muted_fg));
            }
        }


        // Footer: only rendered in framed (selector) mode.  The non-framed
        // REPL path puts all metadata in the forgecode-style header above.
        let footer = if !framed {
            String::new()
        } else {
            let left = if self.usage.total_tokens > 0 {
                format!(
                    "\u{2191}{} \u{2193}{} ${:.4}",
                    format_tokens(self.usage.input),
                    format_tokens(self.usage.output),
                    self.usage.cost.total
                )
            } else {
                String::new()
            };
            let mut right = format!(
                "phi \u{00b7} {} \u{00b7} thinking:{}",
                self.model_label,
                self.root_lm.thinking().as_str()
            );
            right.push_str(" \u{00b7} ");
            right.push_str(&pwd);
            if let Some(branch) = &self.branch {
                right.push_str(" \u{00b7} ");
                right.push_str(branch);
            }
            match (left.is_empty(), right.is_empty()) {
                (true, true) => String::new(),
                (true, false) => right,
                (false, true) => left,
                (false, false) => format!("{left} \u{00b7} {right}"),
            }
        };
        if framed {
            let footer_fill = width.saturating_sub(footer.chars().count() + 5).max(1);
            lines.push(format!(
                "{chrome_border}\u{2514}\u{2500} {muted_fg}{footer}{RESET} {}\u{2518}{RESET}",
                "\u{2500}".repeat(footer_fill),
                chrome_border = &p.chrome_border,
                muted_fg = &p.muted_fg,
            ));
        }
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
        let widget = Selector::new(format!("Usage · {} entries", report.entries), items, false);
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
                let detail = self
                    .registry
                    .find(name)
                    .map(|model| format!("[{provider}] {}", format_model_pricing(model)))
                    .unwrap_or_else(|| format!("[{provider}]"));
                SelectorItem::new(name.clone(), id.to_owned())
                    .detail(detail)
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
            Err(e) => self.pending.push(self.err(&e)),
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
            Err(e) => self.pending.push(self.err(&e)),
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
        let _ = self.store.set_leaf(leaf);
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
        self.clear_transcript();
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
                    let text = match entry.role {
                        session::Role::Assistant => entry
                            .display_text
                            .clone()
                            .unwrap_or_else(|| entry.text.clone()),
                        session::Role::User => entry.text.clone(),
                    };
                    ReplayItem::Message(entry.role, text)
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
                } => {
                    self.push_repl_replay(&code, &stdout, &stderr, duration_secs, &tools, &subcalls)
                }
            }
        }
        // A previous run died mid-turn (exchange_start without its
        // exchange_end): say so instead of silently presenting a transcript
        // that ends earlier than the user remembers, then durably close the
        // exchange so the notice appears once.
        if self.store.interrupted_exchange().is_some() {
            self.pending.push(String::new());
            self.pending.push(format!(
                "{}previous run was interrupted mid-turn; completed work up to the last checkpoint is shown above{RESET}",
                &self.palette.warning_fg
            ));
            self.last_activity = false;
            let _ = self.store.acknowledge_interrupted();
        }
    }

    fn push_repl_replay(
        &mut self,
        code: &str,
        stdout: &str,
        stderr: &str,
        _duration_secs: f64,
        tools: &[session::ToolCall],
        subcalls: &[session::SubcallRecord],
    ) {
        self.begin_block();
        self.push_repl_block(0, code, self.code_limit());
        // Replay the persisted tool calls and sub-LLM calls nested in the
        // block, mirroring the live ToolStart/ToolEnd rendering so a
        // resumed session looks like the one the user actually saw.
        for tool in tools {
            let depth = tool.depth + 1;
            match &tool.error {
                Some(error) => {
                    self.push_tool_result(
                        depth,
                        &tool.name,
                        &tool.args,
                        error,
                        self.output_limit(),
                        None,
                        true,
                    );
                }
                None => {
                    self.push_tool_result(
                        depth,
                        &tool.name,
                        &tool.args,
                        &tool.result,
                        self.output_limit(),
                        tool_preview_lang(&tool.name, &tool.args),
                        false,
                    );
                }
            }
        }
        for subcall in subcalls {
            let depth = subcall.depth;
            let label = subcall_title(&subcall.model, &subcall.prompt_preview, &self.palette);
            let content = subcall
                .error
                .as_deref()
                .or(subcall.result_preview.as_deref());
            let rows = self.subcall_result_rows(
                depth,
                &label,
                content,
                self.output_limit(),
                subcall.error.is_some(),
                None,
            );
            for row in rows {
                self.push_row(row);
            }
            self.last_activity = true;
        }
        self.last_activity = true;
        let filtered = strip_tool_echoes(stdout, tools, 0);
        self.push_preview(0, &filtered, self.output_limit());
        self.push_preview(0, stderr, self.output_limit());
        self.commit_block();
    }

    /// Render a persisted assistant message the way it looked live: optional
    /// provider thinking, prose as markdown, and each ```repl block shown as a
    /// committed repl container (code preview over the repl background)
    /// rather than raw fenced text.
    fn push_assistant_message(&mut self, text: &str) {
        let (thinking, text) = split_assistant_display_text(text);
        if let Some(thinking) = thinking
            && self.expand_thinking
        {
            self.push_dot(0, "thinking".to_owned());
            self.push_preview(0, thinking, usize::MAX);
        }

        let blocks = find_code_blocks(text);
        let segments = split_prose(text);
        for (i, segment) in segments.iter().enumerate() {
            self.push_prose(segment);
            if let Some(code) = blocks.get(i) {
                self.begin_block();
                self.push_repl_block(0, code, self.code_limit());
                self.commit_block();
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

    fn replay_transcript_if_idle(&mut self) {
        if self.running.is_none() && !self.block_active {
            self.replay_branch();
        }
    }

    /// ctrl+t: collapse or expand provider thinking throughout the transcript.
    fn toggle_thinking(&mut self) {
        self.expand_thinking = !self.expand_thinking;
        self.replay_transcript_if_idle();
    }

    fn toggle_tools(&mut self) {
        self.expand_tools = !self.expand_tools;
        self.replay_transcript_if_idle();
        self.pending.push(format!(
            "{DIM}[tool output {} throughout transcript]{RESET}",
            if self.expand_tools {
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
            && let Some(popup) = complete(
                &before,
                &self.model_names,
                &self.runtime.list_extension_commands(),
            )
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
    async fn handle_input(&mut self, event: Event, cmd_tx: &mpsc::UnboundedSender<Cmd>) -> bool {
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
                    self.toggle_tools();
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
                self.clear_transcript();
                let _ = self.term.clear_all();
                match SessionStore::new(&self.cwd) {
                    Ok(store) => self.store = store,
                    Err(e) => self.pending.push(self.err(&e)),
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
                    Err(e) => self.pending.push(self.err(&e)),
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
            // Extension slash commands: dispatch to the registered handler
            // via the runtime (e.g. /recall, /capture, /explore from
            // builtin.memory). Anything that isn't a built-in or shell
            // escape and starts with '/' lands here.
            _ if trimmed.starts_with('/') => {
                let (cmd_name, args) = match trimmed.split_once(' ') {
                    Some((name, rest)) => (name, rest),
                    None => (trimmed, ""),
                };
                // Normalize a leading run of slashes (e.g. `//memories`) down
                // to a single slash so the command is looked up by the name
                // it was registered with.
                let cmd_name = {
                    let bare = cmd_name.trim_start_matches('/');
                    format!("/{bare}")
                };
                let args = args.to_owned();
                let runtime = Arc::clone(&self.runtime);
                let result = tokio::task::block_in_place(|| {
                    let rt = runtime;
                    let cn = cmd_name.clone();
                    let ar = args.clone();
                    tokio::runtime::Handle::current()
                        .block_on(async move { rt.invoke_command(&cn, &ar).await })
                });
                match result {
                    Ok(Some(output)) => {
                        if let Some(msg) = output.message {
                            self.pending.push(String::new());
                            self.pending.push(msg);
                        }
                    }
                    Ok(None) => {
                        self.pending
                            .push(self.warn(&format!("unknown command: {cmd_name}")));
                    }
                    Err(e) => {
                        self.pending.push(self.err(&e));
                    }
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
            self.push_repl_block(0, &code, self.code_limit());
        } else {
            self.pending.push(String::new());
            self.pending
                .push(format!("{tool}${RESET} {DIM}{cmd}{RESET}", tool = &self.palette.tool_accent));
            self.last_activity = false;
        }
        let _ = cmd_tx.send(Cmd::Shell { code, record });
    }

    fn send_prompt(&mut self, prompt: &str, cmd_tx: &mpsc::UnboundedSender<Cmd>) {
        self.push_user_message(prompt);
        if let Err(e) = self.store.append(session::Role::User, prompt) {
            self.pending
                .push(format!("{}session not saved:{RESET} {e}", &self.palette.warning_fg));
        }
        // Durable turn boundary: an exchange_start without its exchange_end
        // is how a crash mid-turn stays detectable after restart.
        let _ = self.store.begin_exchange();
        let cancel = CancellationToken::new();
        self.running = Some(cancel.clone());
        self.turn = 0;
        self.stream.clear();
        self.thinking.clear();
        self.thinking_log.clear();
        self.status = "thinking...".to_owned();
        let _ = cmd_tx.send(Cmd::Prompt {
            text: prompt.to_owned(),
            cancel,
        });
    }

    fn switch_model(&mut self, query: &str, root: bool) {
        let Some(model) = self.registry.find(query).cloned() else {
            self.pending.push(self.err(
                &format!("unknown model '{query}' (tab after /model lists models)")
            ));
            return;
        };
        if !self.registry.is_supported(&model) {
            self.pending.push(self.err(
                &format!("model '{}/{}' is not supported", model.provider, model.id)
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
            self.pending.push(self.err(
                &format!("unknown provider '{name}' (expected: anthropic, openai)")
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
                        .push(format!("{}login failed:{RESET} {e}", &self.palette.error_fg)),
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

    fn panel_width(&self) -> usize {
        self.term.width().max(1)
    }

    /// Display columns available to rows inside the outer `repl` panel.
    fn repl_body_width(&self) -> usize {
        self.panel_width().saturating_sub(4).max(4)
    }

    /// Display columns available to a flat row's result body at `depth`: the
    /// repl body minus the result-gutter width (`result_prefix_width` =
    /// `indent_width(depth) + 2`). Body content is wrapped to this so each
    /// indented result line fits within the repl body.
    fn card_body_width(&self, depth: usize) -> usize {
        self.repl_body_width()
            .saturating_sub(result_prefix_width(depth))
            .max(1)
    }

    /// Assistant prose: a flat `Assistant` header + plain markdown body. No
    /// box, no result indent — prose is the model's primary output, not nested
    /// secondary content.
    fn assistant_card_lines(&self, text: &str) -> Vec<String> {
        let rendered = render_markdown(text, self.markdown_width(), &self.palette.markdown);
        let mut out = vec![forge_title(0, None, "Assistant")];
        for line in rendered {
            out.push(if line.is_empty() {
                String::new()
            } else {
                format!("{fg}{line}{RESET}", fg = &self.palette.assistant_fg)
            });
        }
        out
    }

    /// User echo: a flat `● You` header + plain body.
    fn user_card_lines(&self, text: &str) -> Vec<String> {
        let rendered = render_markdown(text, self.markdown_width(), &self.palette.markdown);
        let mut out = vec![forge_title(0, None, "You")];
        for line in rendered {
            out.push(if line.is_empty() {
                String::new()
            } else {
                format!("{fg}{line}{RESET}", fg = &self.palette.user_fg)
            });
        }
        out
    }

    /// Live/scrollback activity rows rendered as a flat `●`-titled unit
    /// nested under the repl block.
    fn tool_lines(&self, depth: usize, title: &str, body: &str, max: usize) -> Vec<String> {
        let mut rows = Vec::new();
        let trimmed = body.trim_end();
        if !trimmed.is_empty() {
            let output_rows: Vec<String> = trimmed
                .lines()
                .flat_map(|line| wrap_ansi(line, self.card_body_width(depth)))
                .collect();
            let shown = output_rows.len().min(max);
            for row in &output_rows[..shown] {
                rows.push(format!("{DIM}{row}{RESET}"));
            }
            if output_rows.len() > max {
                rows.push(format!(
                    "{DIM}… ({} more lines){RESET}",
                    output_rows.len() - max
                ));
            }
        }

        render_flat(None, title, depth, &rows)
    }

    fn repl_code_lines(&self, depth: usize, body: &str, max: usize) -> Vec<String> {
        let trimmed = body.trim_end();
        let mut rows = Vec::new();
        if !trimmed.is_empty() {
            rows = highlight_code(trimmed, Some("repl"), &self.palette.markdown)
                .into_iter()
                .flat_map(|line| wrap_ansi(&line, self.card_body_width(depth)))
                .collect();
            if rows.len() > max {
                let hidden = rows.len() - max;
                rows.truncate(max);
                rows.push(format!("{DIM}… ({hidden} more lines){RESET}"));
            }
        }
        let prefix = result_prefix(depth);
        let blank = indent(depth);
        rows.into_iter()
            .map(|line| {
                if line.is_empty() {
                    blank.clone()
                } else {
                    format!("{prefix}{line}")
                }
            })
            .collect()
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
            Vec::new()
        }
    }

    fn flush_thinking(&mut self) {
        let thinking = std::mem::take(&mut self.thinking);
        if thinking.trim().is_empty() {
            return;
        }
        if self.expand_thinking {
            self.push_dot(0, "thinking".to_owned());
            self.push_preview(0, &thinking, usize::MAX);
        }
    }

    /// In-flight activity row: a single flat line prefixed by the nesting
    /// gutter, carrying the animated spinner + label (no `●` — the spinner is
    /// the live indicator; `●` is reserved for committed headers).
    fn status_rows(&self, depth: usize, text: &str) -> Vec<String> {
        vec![format!("{}{text}", indent(depth))]
    }

    fn push_status(&mut self, depth: usize, text: String) -> Option<(usize, usize)> {
        if depth == 0 && !self.last_activity && !self.block_active {
            self.pending.push(String::new());
        }
        let rows = self.status_rows(depth, &text);
        let start = if self.block_active {
            Some(self.block.len())
        } else {
            None
        };
        let len = rows.len();
        for row in rows {
            self.push_row(row);
        }
        self.last_activity = true;
        start.map(|start| (start, len))
    }

    fn shift_open_row_starts(&mut self, after: usize, delta: isize) {
        if delta == 0 {
            return;
        }
        for row in self.open_tool_rows.values_mut() {
            if row.start >= after {
                row.start = ((row.start as isize) + delta) as usize;
            }
        }
        for row in self.open_subcall_rows.iter_mut() {
            if row.start >= after {
                row.start = ((row.start as isize) + delta) as usize;
            }
        }
    }

    fn replace_block_rows(&mut self, start: usize, len: usize, rows: Vec<String>) -> usize {
        if !self.block_active || start > self.block.len() {
            return rows.len();
        }
        let end = (start + len).min(self.block.len());
        let old_len = end.saturating_sub(start);
        let new_len = rows.len();
        self.block.splice(start..end, rows);
        self.shift_open_row_starts(end, new_len as isize - old_len as isize);
        new_len
    }

    fn refresh_open_activity_rows(&mut self) {
        if !self.block_active {
            return;
        }
        let spinner = self.spinner.frame().to_owned();

        let mut tool_ids: Vec<((usize, u64), usize)> = self
            .open_tool_rows
            .iter()
            .map(|(&id, row)| (id, row.start))
            .collect();
        tool_ids.sort_by_key(|(_, start)| std::cmp::Reverse(*start));
        for (id, _) in tool_ids {
            let Some(row) = self.open_tool_rows.get(&id).cloned() else {
                continue;
            };
            let new_len = self.replace_block_rows(
                row.start,
                row.len,
                self.status_rows(row.depth, &running_status_text(&row.label, &spinner)),
            );
            if let Some(open) = self.open_tool_rows.get_mut(&id) {
                open.len = new_len;
            }
        }

        let mut sub_idxs: Vec<(usize, usize)> = self
            .open_subcall_rows
            .iter()
            .enumerate()
            .map(|(idx, row)| (idx, row.start))
            .collect();
        sub_idxs.sort_by_key(|(_, start)| std::cmp::Reverse(*start));
        for (idx, _) in sub_idxs {
            let Some(row) = self.open_subcall_rows.get(idx).cloned() else {
                continue;
            };
            let new_len = self.replace_block_rows(
                row.start,
                row.len,
                self.status_rows(row.depth, &running_status_text(&row.label, &spinner)),
            );
            if let Some(open) = self.open_subcall_rows.get_mut(idx) {
                open.len = new_len;
            }
        }
    }

    fn fail_open_activity_rows(&mut self) {
        let mut tool_ids: Vec<((usize, u64), usize)> = self
            .open_tool_rows
            .iter()
            .map(|(&id, row)| (id, row.start))
            .collect();
        tool_ids.sort_by_key(|(_, start)| std::cmp::Reverse(*start));
        for (id, _) in tool_ids {
            let Some(row) = self.open_tool_rows.remove(&id) else {
                continue;
            };
            self.replace_block_rows(
                row.start,
                row.len,
                self.status_rows(row.depth, &failed_status_text(&row.label, None, None, &self.palette)),
            );
        }
        while let Some(row) = self.open_subcall_rows.pop_back() {
            self.replace_block_rows(
                row.start,
                row.len,
                self.status_rows(row.depth, &failed_status_text(&row.label, None, None, &self.palette)),
            );
        }
    }

    /// Append an action row to the active block buffer, or straight to
    /// scrollback when no block is in flight.
    fn push_row(&mut self, row: String) {
        if self.block_active {
            self.block.push(row);
        } else {
            self.pending.push(row);
        }
    }

    /// Start buffering the depth-0 REPL block that is about to execute. Rows
    /// pushed until `commit_block` render live as a grouped flat `● repl` unit.
    fn begin_block(&mut self) {
        if !self.last_activity {
            self.pending.push(String::new());
        }
        self.block.clear();
        self.open_tool_rows.clear();
        self.open_subcall_rows.clear();
        self.block_active = true;
    }

    /// Commit the buffered block to scrollback as a flat `● repl` unit, followed
    /// by any prose that streamed in behind it.
    fn commit_block(&mut self) {
        if !self.block_active {
            return;
        }
        self.block_active = false;
        let body = std::mem::take(&mut self.block);
        // The `repl · turn N` label belongs to the *live* action block built
        // during an iteration (running is Some). Persisted assistant messages
        // re-rendered as prose containers (`push_assistant_message`, during
        // restore or after a run completes) are a different artifact with no
        // meaningful per-block turn, so they stay at a plain `repl` label.
        let turn = self.running.is_some().then_some(self.turn);
        self.pending.extend(render_repl_block(&body, turn));
        self.open_tool_rows.clear();
        self.open_subcall_rows.clear();
        let after = std::mem::take(&mut self.after_block);
        self.pending.extend(after);
    }

    /// First line of an action block — a flat `● [time] title` header, the
    /// forgecode-style anchor for a tool/thinking unit. A depth-0 action that
    /// follows prose gets a separating blank line first. `dot_color` carries
    /// the category; `title` is the already-styled text.
    fn push_dot(&mut self, depth: usize, text: String) {
        if depth == 0 && !self.last_activity && !self.block_active {
            self.pending.push(String::new());
        }
        self.push_row(forge_title(
            depth,
            self.committed_ts().as_deref(),
            &text,
        ));
        self.last_activity = true;
    }

    /// Continuation/result line inside an action block, rendered as a flat
    /// `●`-prefixed line under the nesting gutter.
    fn push_bar(&mut self, depth: usize, text: String) {
        let rows = render_flat(None, &format!("{DIM}{text}{RESET}"), depth, &[]);
        for row in rows {
            self.push_row(row);
        }
        self.last_activity = true;
    }

    /// Indented code/output preview inside an action block: at most `max`
    /// visual rows (counting wraps), then a `… (n more lines)` marker.
    fn push_preview(&mut self, depth: usize, text: &str, max: usize) {
        self.push_preview_with_lang(depth, text, max, None);
    }

    fn preview_rows_with_lang(
        &self,
        depth: usize,
        text: &str,
        max: usize,
        lang: Option<&str>,
    ) -> Vec<String> {
        let trimmed = text.trim_end();
        if trimmed.is_empty() {
            return Vec::new();
        }
        let base_rows: Vec<String> = match lang {
            Some(lang) => highlight_code(trimmed, Some(lang), &self.palette.markdown),
            None => trimmed
                .lines()
                .map(|line| {
                    highlight_diff_line(line, &self.palette.markdown)
                        .unwrap_or_else(|| highlight_output(line, &self.palette.markdown))
                })
                .collect(),
        };
        let mut rows: Vec<String> = base_rows
            .into_iter()
            .flat_map(|line| wrap_ansi(&line, self.card_body_width(depth)))
            .collect();
        if rows.len() > max {
            let hidden = rows.len() - max;
            rows.truncate(max);
            rows.push(format!("{DIM}… ({hidden} more lines){RESET}"));
        }
        rows
    }

    fn push_preview_with_lang(&mut self, depth: usize, text: &str, max: usize, lang: Option<&str>) {
        let rows = self.preview_rows_with_lang(depth, text, max, lang);
        if rows.is_empty() {
            return;
        }
        let prefix = result_prefix(depth);
        let blank = indent(depth);
        for line in &rows {
            self.push_row(if line.is_empty() {
                blank.clone()
            } else {
                format!("{prefix}{line}")
            });
        }
    }

    /// `Some` while a turn is live (so committed headers stamp a `[HH:MM:SS]`),
    /// `None` on replay/restore (no false replay-time clock). See `forge_title`.
    fn committed_ts(&self) -> Option<String> {
        self.running.is_some().then(now_hms)
    }

    #[allow(clippy::too_many_arguments)]
    fn tool_result_rows(
        &self,
        depth: usize,
        name: &str,
        args: &serde_json::Value,
        output: &str,
        max: usize,
        lang: Option<&str>,
        _failed: bool,
        ts: Option<&str>,
    ) -> Vec<String> {
        let identity = tool_identity(name, args, &self.palette);
        // The colored `●` carries success/failure (green/red); the in-flight
        // spinner label uses the same anchor via `tool_title`, so running and
        // committed render the same identity (no flicker on done). The header
        // carries the canonical identity (path/command/pattern) once; the body
        // folds multi-line detail (full bash command / repl code) under it. For
        // tools whose identity IS the anchor, the body is empty and only the
        // output preview shows.
        let width = self.card_body_width(depth);
        let mut body: Vec<String> = identity
            .body
            .into_iter()
            .flat_map(|line| wrap_ansi(&line, width))
            .collect();
        body.extend(self.preview_rows_with_lang(depth, output, max, lang));
        render_flat(ts, &identity.anchor, depth, &body)
    }

    #[allow(clippy::too_many_arguments)]
    fn subcall_result_rows(
        &self,
        depth: usize,
        label: &str,
        content: Option<&str>,
        max: usize,
        _failed: bool,
        ts: Option<&str>,
    ) -> Vec<String> {
        let body = content
            .map(|text| self.preview_rows_with_lang(depth, text, max, None))
            .unwrap_or_default();
        render_flat(ts, label, depth, &body)
    }

    #[allow(clippy::too_many_arguments)]
    fn push_tool_result(
        &mut self,
        depth: usize,
        name: &str,
        args: &serde_json::Value,
        output: &str,
        max: usize,
        lang: Option<&str>,
        failed: bool,
    ) {
        if depth == 0 && !self.last_activity && !self.block_active {
            self.pending.push(String::new());
        }
        let rows = self.tool_result_rows(
            depth,
            name,
            args,
            output,
            max,
            lang,
            failed,
            self.committed_ts().as_deref(),
        );
        for row in rows {
            self.push_row(row);
        }
        self.last_activity = true;
    }

    fn push_repl_block(&mut self, depth: usize, text: &str, max: usize) {
        for row in self.repl_code_lines(depth, text, max) {
            self.push_row(row);
        }
        self.last_activity = true;
    }

    /// Echo a user message into scrollback as a pi-style container: full-width
    /// `userMessageBg` fill with one cell of padding on every side.
    fn push_user_message(&mut self, text: &str) {
        self.pending.push(String::new());
        self.pending.extend(self.user_card_lines(text));
        self.last_activity = false;
    }

    /// Render finalized prose (markdown) into scrollback. While a block is
    /// in flight the prose waits behind it so scrollback stays chronological.
    fn push_prose(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let card = self.assistant_card_lines(text);
        let sink = if self.block_active {
            &mut self.after_block
        } else {
            &mut self.pending
        };
        sink.push(String::new());
        sink.extend(card);
        self.last_activity = false;
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
                if iteration == 0 {
                    self.thinking_log.clear();
                } else if !self.thinking_log.trim().is_empty() {
                    self.thinking_log.push_str("\n\n");
                }
                self.streamed = false;
            }
            RlmEvent::RootDelta {
                depth: 0, delta, ..
            } => {
                self.stream.push_str(&delta);
                self.streamed = true;
                // Keep streamed assistant prose live-only. The durable JSONL
                // assistant message is appended at run completion, so the
                // retained transcript must also commit prose at that message
                // boundary instead of turning paragraph-stable deltas into
                // separate Assistant cards.
            }
            RlmEvent::RootThinkingDelta {
                depth: 0, delta, ..
            } => {
                self.thinking.push_str(&delta);
                self.thinking_log.push_str(&delta);
            }
            RlmEvent::RootText { depth: 0, text, .. } => {
                self.flush_thinking();
                // Commit the iteration's prose as durable cards, splitting on
                // fences so each becomes one assistant card (apart from the
                // explicit REPL/code cards). Prose preceding each executed
                // block was already committed eagerly by `CodeExecStart`, which
                // consumed it from `stream` to keep it ordered above the block;
                // for a streamed message only the uncommitted tail left in
                // `stream` may be flushed here. Re-splitting the full `text`
                // would re-render those earlier segments a second time, leaving
                // duplicated prose trailing each code block. When the model did
                // not stream, `stream` is empty and `RootText` carries the only
                // copy, so fall back to `text`.
                let source = if self.streamed {
                    std::mem::take(&mut self.stream)
                } else {
                    text
                };
                for segment in split_prose(&source) {
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
                    self.live_tools.clear();
                    self.live_subcalls.clear();
                    self.open_tool_args.clear();
                    self.open_tool_rows.clear();
                    self.open_subcall_rows.clear();
                    self.begin_block();
                    self.live_code = Some(code.clone());
                }
                self.push_repl_block(depth, &code, self.code_limit());
            }
            RlmEvent::CodeExecEnd {
                depth,
                stdout,
                stderr,
                duration,
                ..
            } => {
                if depth == 0 {
                    // Checkpoint the completed step as it happens (not at
                    // exchange end): a crash later in the turn must not lose
                    // work already paid for. `branch_restore` replays these
                    // to rebuild the REPL env.
                    self.status = "thinking...".to_owned();
                    self.open_tool_args.clear();
                    self.open_tool_rows.clear();
                    self.open_subcall_rows.clear();
                    if let Some(code) = self.live_code.take() {
                        let duration_secs = duration.as_secs_f64();
                        let tools = std::mem::take(&mut self.live_tools);
                        let subcalls = std::mem::take(&mut self.live_subcalls);
                        let _ = self.store.append_repl(session::ReplRecord {
                            code: code.clone(),
                            stdout: stdout.clone(),
                            stderr: stderr.clone(),
                            duration_secs,
                            kind: session::ReplKind::Agent,
                            tools: tools.clone(),
                            subcalls: subcalls.clone(),
                        });
                        // Discard the incrementally-built live block and rebuild
                        // from the stored record so live and replay use the same
                        // rendering path.
                        self.block.clear();
                        self.block_active = false;
                        self.after_block.clear();
                        self.push_repl_replay(
                            &code,
                            &stdout,
                            &stderr,
                            duration_secs,
                            &tools,
                            &subcalls,
                        );
                    } else {
                        self.live_tools.clear();
                        self.live_subcalls.clear();
                        self.commit_block();
                    }
                } else {
                    self.last_activity = true;
                    let filtered = strip_tool_echoes(&stdout, &self.live_tools, depth);
                    self.push_preview(depth, &filtered, self.output_limit());
                    self.push_preview(depth, &stderr, self.output_limit());
                }
            }
            // Prelude tools run inside the REPL block at `depth`, so they
            // render one nested box level deeper, nested in that block.
            RlmEvent::ToolStart {
                depth,
                call_id,
                name,
                args,
            } => {
                let key = (depth, call_id);
                if self.live_code.is_some() {
                    self.open_tool_args.insert(key, args.clone());
                }
                let label = tool_title(&name, &args, &self.palette);
                if let Some((start, len)) =
                    self.push_status(depth + 1, running_status_text(&label, self.spinner.frame()))
                {
                    self.open_tool_rows.insert(
                        key,
                        OpenToolRow {
                            depth: depth + 1,
                            label,
                            start,
                            len,
                        },
                    );
                }
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
                let key = (depth, call_id);
                let args = self.open_tool_args.remove(&key).unwrap_or_default();
                if self.live_code.is_some() {
                    // Replay only ever renders a short preview, and the full
                    // text already lands in the repl stdout payload, so cap
                    // what gets persisted per tool call.
                    self.live_tools.push(session::ToolCall {
                        name: name.clone(),
                        args: args.clone(),
                        result: truncate_for_log(&result),
                        error: error.as_deref().map(truncate_for_log),
                        duration_secs: duration.as_secs_f64(),
                        depth,
                    });
                }
                let output = error.as_deref().unwrap_or(&result);
                let lang = if error.is_some() {
                    None
                } else {
                    tool_preview_lang(&name, &args)
                };
                let rows = self.tool_result_rows(
                    depth + 1,
                    &name,
                    &args,
                    output,
                    self.output_limit(),
                    lang,
                    error.is_some(),
                    self.committed_ts().as_deref(),
                );
                if let Some(row) = self.open_tool_rows.remove(&key) {
                    self.replace_block_rows(row.start, row.len, rows);
                    self.last_activity = true;
                } else {
                    for row in rows {
                        self.push_row(row);
                    }
                    self.last_activity = true;
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
                let label = subcall_title(&model, &prompt_preview, &self.palette);
                if let Some((start, len)) =
                    self.push_status(depth, running_status_text(&label, self.spinner.frame()))
                {
                    self.open_subcall_rows.push_back(OpenSubcallRow {
                        depth,
                        prompt_preview: prompt_preview.clone(),
                        label,
                        start,
                        len,
                    });
                }
                if depth == 0 {
                    self.status = format!("waiting on {model}...");
                }
            }
            RlmEvent::SubcallEnd {
                depth,
                model,
                result_preview,
                duration,
                error,
            } => {
                let open = self
                    .open_subcall_rows
                    .iter()
                    .position(|row| row.depth == depth)
                    .and_then(|idx| self.open_subcall_rows.remove(idx));
                if self.live_code.is_some() {
                    let prompt_preview = open
                        .as_ref()
                        .map(|row| row.prompt_preview.clone())
                        .unwrap_or_default();
                    self.live_subcalls.push(session::SubcallRecord {
                        model: model.clone(),
                        prompt_preview,
                        result_preview: result_preview.clone(),
                        error: error.clone(),
                        duration_secs: duration.as_secs_f64(),
                        depth,
                    });
                }
                // Reuse the label formatted once at `SubcallStart` (stored on
                // the open row) instead of recomputing it for the border.
                let label = open
                    .as_ref()
                    .map(|row| row.label.clone())
                    .unwrap_or_else(|| subcall_title(&model, "", &self.palette));
                let content = error.as_deref().or(result_preview.as_deref());
                let rows = self.subcall_result_rows(
                    depth,
                    &label,
                    content,
                    self.output_limit(),
                    error.is_some(),
                    self.committed_ts().as_deref(),
                );
                if let Some(row) = open {
                    self.replace_block_rows(row.start, row.len, rows);
                    self.last_activity = true;
                } else {
                    if depth == 0 && !self.last_activity && !self.block_active {
                        self.pending.push(String::new());
                    }
                    for row in rows {
                        self.push_row(row);
                    }
                    self.last_activity = true;
                }
            }
            RlmEvent::RunEnd {
                depth,
                stop,
                iterations,
                duration,
            } if depth > 0 => {
                // A recursive subcall's header is the `SubcallStart` card; skip
                // the redundant `RunStart` "rlm" card (it would double-mark the
                // same nesting level). The run's outcome is summarized below.
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
            _ => {}
        }
    }

    fn handle_done(&mut self, done: Done, cmd_tx: &mpsc::UnboundedSender<Cmd>) {
        match done {
            Done::Run(completion) => self.handle_run_done(completion),
            Done::Shell {
                code,
                record,
                result,
            } => self.handle_shell_done(code, record, result),
            Done::Restored(result) => {
                if let Err(e) = result {
                    self.pending
                        .push(format!("{}restore failed:{RESET} {e}", &self.palette.error_fg));
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

    fn handle_shell_done(
        &mut self,
        code: String,
        record: bool,
        result: Result<ReplResult, String>,
    ) {
        match result {
            Ok(res) if record => {
                self.last_activity = true;
                self.push_preview(0, &res.stdout, self.output_limit());
                self.push_preview(0, &res.stderr, self.output_limit());
                self.commit_block();
                let _ = self.store.append_repl(session::ReplRecord {
                    code,
                    stdout: res.stdout,
                    stderr: res.stderr,
                    duration_secs: res.duration.as_secs_f64(),
                    kind: session::ReplKind::Shell,
                    tools: Vec::new(),
                    subcalls: Vec::new(),
                });
            }
            Ok(res) => {
                let output = format!("{}{}", res.stdout, res.stderr);
                if output.trim().is_empty() {
                    self.pending.push(format!("{DIM}(no output){RESET}"));
                } else {
                    self.pending
                        .extend(output.trim_end().lines().map(str::to_owned));
                }
                self.last_activity = false;
            }
            Err(e) => {
                if self.block_active {
                    self.push_bar(0, format!("failed: {e}"));
                    self.commit_block();
                } else {
                    self.pending.push(self.err(&e));
                    self.last_activity = false;
                }
            }
        }
    }

    fn handle_run_done(&mut self, completion: RlmCompletion) {
        // A block still open here never finished (cancel/failure mid-exec).
        self.fail_open_activity_rows();
        self.commit_block();
        self.live_code = None;
        self.running = None;
        self.stream.clear();
        self.thinking.clear();
        let display_text = assistant_display_text(&completion.response, &self.thinking_log);
        self.thinking_log.clear();
        // Mirror the env's history condition so the store and the REPL's
        // history_N variables stay aligned. Store before rendering so the
        // log is durable before the display reflects it.
        if completion.stop != StopCause::Failed && !completion.response.trim().is_empty() {
            let usage = session::SessionUsage {
                input: completion.usage.input,
                output: completion.usage.output,
                cache_read: completion.usage.cache_read,
                cache_write: completion.usage.cache_write,
                cost_total: completion.usage.cost.total,
                model: "__total__".to_owned(),
            };
            let usage_by_model = completion
                .usage_by_model
                .iter()
                .map(|breakdown| session::SessionUsage {
                    input: breakdown.usage.input,
                    output: breakdown.usage.output,
                    cache_read: breakdown.usage.cache_read,
                    cache_write: breakdown.usage.cache_write,
                    cost_total: breakdown.usage.cost.total,
                    model: breakdown.model.clone(),
                })
                .collect();
            let _ = self.store.append_with_usage_breakdown_and_display(
                session::Role::Assistant,
                &completion.response,
                display_text,
                Some(usage),
                usage_by_model,
            );
        }
        self.pending.push(String::new());
        if completion.stop == StopCause::Failed {
            let error = completion.error.unwrap_or_else(|| "run failed".to_owned());
            self.pending.push(self.err(&error));
        } else if !completion.response.is_empty() {
            self.push_assistant_message(&completion.response);
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
        self.last_activity = false;
    }

    fn markdown_width(&self) -> usize {
        self.term.width().saturating_sub(6).min(100)
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

    fn test_ui(width: u16, height: u16) -> Ui {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        std::mem::forget(dir);

        let storage = Arc::new(AuthStorage::new(cwd.join("auth.json")));
        let registry = Arc::new(Registry::builtin());
        let model = registry
            .supported_models()
            .next()
            .expect("builtin registry has models")
            .clone();
        let root_lm = Arc::new(ProviderLm::new(
            registry.clone(),
            model.clone(),
            storage.clone(),
            ThinkingLevel::Off,
            None,
        ));
        let sub_lm = Arc::new(ProviderLm::new(
            registry.clone(),
            model,
            storage.clone(),
            ThinkingLevel::Off,
            None,
        ));
        let mut store = SessionStore::new(&cwd).unwrap();
        // Redirect under the temp dir so tests never touch the real
        // ~/.local/share/phi/sessions data dir (mirrors `store_in` and
        // phi-mux's `write_session`).
        store.set_test_path(cwd.join(format!("{}.jsonl", store.id())));
        let runtime = Arc::new(
            phi_runtime::RuntimeBuilder::new()
                .with_cwd(&cwd)
                .build()
                .unwrap(),
        );
        let (login_tx, _login_rx) = mpsc::unbounded_channel();

        Ui::new(
            Terminal::new(
                Box::new(Vec::<u8>::new()) as Box<dyn std::io::Write + Send>,
                width,
                height,
            ),
            UiSetup {
                max_iterations: 8,
                storage,
                registry,
                root_lm,
                sub_lm,
                store,
            },
            login_tx,
            cwd,
            None,
            runtime,
        )
    }

    #[test]
    fn subcall_title_flattens_multiline_preview() {
        let title = subcall_title("model-x", "first line\n\nsecond\tline", &InlinePalette::default_const());
        assert!(title.contains("\"first line second line\""), "{title}");
        assert!(!title.contains('\n'), "{title}");
        assert!(!title.contains('\t'), "{title}");
    }

    #[test]
    fn panel_row_does_not_paint_box_body_background() {
        let p = InlinePalette::default_const();
        let row = panel_row("\x1b[31mred\x1b[0m", 12, &p.user_border);
        assert!(!row.contains("\x1b[48;"), "{row:?}");
        assert_eq!(visible_width(&row), 12, "{row:?}");
    }

    #[test]
    fn panel_border_reopens_color_after_label_reset() {
        let p = InlinePalette::default_const();
        let border = panel_border(&format!("{BOLD}Input{RESET}"), "Input", 12, &p.user_border);
        assert!(
            border.contains(&format!("{RESET}{}", p.user_border)),
            "{border:?}"
        );
    }

    // Flat form: a bare header line (no trailing `─…─` rule) plus a body
    // indented under a 2-space result gutter. No glyph — the header is just
    // the title at the nesting indent, the body is plain indented text.
    #[test]
    fn render_flat_is_a_bare_header_with_indented_body() {
        let lines = render_flat(None, "read foo.rs", 1, &["body one".to_owned()]);
        let plain: Vec<String> = lines.iter().map(|l| phi_tui::strip_ansi(l)).collect();
        // Header at depth 1: 2-space indent + title, no glyph.
        assert_eq!(plain[0], "  read foo.rs", "{plain:?}");
        assert!(!plain[0].contains('⎿'), "header must not carry ⎿: {plain:?}");
        assert!(!plain[0].contains('─'), "no trailing rule fill: {plain:?}");
        // Result body: indent(1)=2 + result gutter 2 = 4 cells, then content.
        assert_eq!(plain[1], "    body one", "{plain:?}");
    }

    // Every body row shares the same plain 2-space result gutter — no glyph
    // on any row, so multi-line results stay flat and uniform.
    #[test]
    fn render_flat_shows_single_crop_for_multiline_body() {
        let lines = render_flat(
            None,
            "read foo.rs",
            1,
            &[
                "line one".to_owned(),
                "line two".to_owned(),
                String::new(),
                "line four".to_owned(),
            ],
        );
        let plain: Vec<String> = lines.iter().map(|l| phi_tui::strip_ansi(l)).collect();
        // No glyph across the whole block.
        assert_eq!(plain.iter().map(|l| l.matches('⎿').count()).sum::<usize>(), 0, "{plain:?}");
        // Every non-empty body row: indent(1)=2 + result gutter 2 = 4 cells.
        assert_eq!(plain[1], "    line one", "{plain:?}");
        assert_eq!(plain[2], "    line two", "{plain:?}");
        // Empty body line collapses to the depth indent (2 at depth 1).
        assert_eq!(plain[3], "  ", "{plain:?}");
        // A non-empty row after a blank keeps the same gutter.
        assert_eq!(plain[4], "    line four", "{plain:?}");
    }

    // forge_title stamps `[HH:MM:SS]` only when given a timestamp; without one
    // (replay/restored rows) the header is bare so re-renders don't show a
    // false replay-time clock.
    #[test]
    fn forge_title_omits_timestamp_when_none() {
        let with_ts = phi_tui::strip_ansi(&forge_title(0, Some("12:00:01"), "read x"));
        let no_ts = phi_tui::strip_ansi(&forge_title(0, None, "read x"));
        assert!(with_ts.contains("[12:00:01]"), "{with_ts}");
        assert!(!no_ts.contains('['), "{no_ts}");
        assert!(!no_ts.contains("●"), "{no_ts}");
    }

    // `indent` keeps the 2-cells-per-depth nesting budget, and the result
    // gutter is a fixed 2 cells. `card_body_width` subtracts
    // `result_prefix_width = indent_width(depth) + 2`, so result bodies fit
    // under their gutter and the math is stable across depths.
    #[test]
    fn indent_and_result_gutter_widths() {
        assert_eq!(indent_width(0), 0);
        assert_eq!(indent_width(1), 2);
        assert_eq!(indent_width(3), 6);
        assert_eq!(visible_width(&indent(3)), 6);
        // Result gutter is 2 cells at every depth (indent + 2).
        for depth in 0..=4 {
            assert_eq!(result_prefix_width(depth), indent_width(depth) + 2, "depth {depth}");
            assert_eq!(visible_width(&result_prefix(depth)), result_prefix_width(depth), "depth {depth}");
        }
        // card_body_width == repl_body_width - result_prefix_width.
        let ui = test_ui(80, 24);
        for depth in 0..=4 {
            assert_eq!(
                ui.card_body_width(depth),
                ui.repl_body_width().saturating_sub(result_prefix_width(depth)).max(1),
                "depth {depth}"
            );
        }
    }

    #[test]
    fn render_flat_clamps_to_available_width() {
        // A tool result committed through a narrow UI: callers wrap the body to
        // card_body_width and the renderer prefixes it under the nesting gutter,
        // so every committed line must fit within the panel width. The header
        // (anchor) is bare content — if the anchor itself overflows that's a
        // caller concern, not a clamp the renderer applies.
        let ui = test_ui(20, 24);
        let lines = ui.tool_result_rows(
            0,
            "read_file",
            &serde_json::json!({"path": "x.rs"}),
            "hello world line that must wrap to fit\n",
            usize::MAX,
            Some("rust"),
            false,
            None,
        );
        let panel = ui.panel_width();
        for line in &lines {
            assert!(
                visible_width(line) <= panel,
                "{} > {} => {}",
                visible_width(line),
                panel,
                line
            );
        }
    }

    // Flat form: one `● read <path>` header (green ● = success, no ✓ glyph,
    // no `─` rule) with the path shown once (the read tool's identity IS the
    // anchor, so the body holds only the output preview). Confirms the anchor
    // is not duplicated as a body line and that no box chrome (┌/├/└) or
    // `Activity`/`Output` section labels appear.
    #[test]
    fn tool_result_merges_activity_and_output_sections() {
        let ui = test_ui(80, 24);
        let lines = ui.tool_result_rows(
            1,
            "read_file",
            &serde_json::json!({"path": "crates/phi-session/src/usage.rs"}),
            "//! usage\n//! details\n",
            usize::MAX,
            Some("rust"),
            false,
            None,
        );
        let plain = lines
            .iter()
            .map(|line| phi_tui::strip_ansi(line))
            .collect::<Vec<_>>()
            .join("\n");

        // Header carries the green ● (no ✓ glyph, no `─` rule) and the anchor
        // (path) once.
        assert!(plain.contains("read"), "{plain}");
        assert!(!plain.contains("✓"), "{plain}");
        assert!(!plain.contains('─'), "{plain}");
        assert!(plain.contains("crates/phi-session/src/usage.rs"), "{plain}");
        // Output preview folds under the header as plain body lines.
        assert!(plain.contains("//! usage"), "{plain}");
        assert!(plain.contains("//! details"), "{plain}");
        // The canonical path appears once — never re-shown as a body line.
        assert_eq!(
            plain.matches("crates/phi-session/src/usage.rs").count(),
            1,
            "{plain}"
        );
        // No four-wall box chrome or section labels.
        assert!(!plain.contains('┌'), "{plain}");
        assert!(!plain.contains('├'), "{plain}");
        assert!(!plain.contains('└'), "{plain}");
        assert!(!plain.contains("Activity"), "{plain}");
        assert!(!plain.contains("Output"), "{plain}");
        // Depth-1 header is inset 2 spaces under the repl block; no `⎿` glyph.
        assert!(plain.lines().any(|l| l.starts_with("  read")), "{plain}");
        assert!(!plain.lines().any(|l| l.contains('⎿')), "{plain}");
    }

    // Flat form for a repl block: no header — code folds under a bare indented
    // body, the exec bar is its own flat line, and stdout folds under it
    // (no `output` label, no `├` separator).
    #[test]
    fn repl_panel_splits_input_and_output_sections() {
        let mut ui = test_ui(80, 24);
        ui.begin_block();
        ui.push_repl_block(0, "print(1)", usize::MAX);
        ui.push_bar(0, "exec 0.1s".to_owned());
        ui.push_preview(0, "1\n", usize::MAX);

        let lines = render_repl_block(&ui.block, Some(1));
        let plain = lines
            .iter()
            .map(|line| phi_tui::strip_ansi(line))
            .collect::<Vec<_>>()
            .join("\n");

        // No repl block header — the code speaks for itself.
        assert!(!plain.contains("repl"), "{plain}");
        // Code and exec bar are present as bare indented rows.
        assert!(plain.contains("print(1)"), "{plain}");
        assert!(plain.contains("exec 0.1s"), "{plain}");
        // The stdout value ("1") is present as a bare row, with no "output"
        // label above it.
        assert!(plain.lines().any(|l| l.trim() == "1"), "{plain}");
        assert!(!plain.contains("output"), "{plain}");
        assert!(!plain.contains("Output"), "{plain}");
        // No box chrome or section separator.
        assert!(!plain.contains('┌'), "{plain}");
        assert!(!plain.contains('├'), "{plain}");
        assert!(!plain.contains('└'), "{plain}");
    }

    // The canonical argument for each tool is shown exactly once across the
    // whole card (header or body, never both), and tools whose identity IS
    // the anchor (read/edit/grep/ls) carry no body — so the path/pattern never
    // re-appears as a body line. `tool_identity` is the single source of truth
    // the in-flight label and the committed card header share.
    #[test]
    fn tool_identity_anchor_shown_once() {
        let p = InlinePalette::default_const();
        let read = tool_identity(
            "read_file",
            &serde_json::json!({"path": "src/parser.rs"}),
            &p,
        );
        assert!(phi_tui::strip_ansi(&read.anchor).contains("read src/parser.rs"));
        assert!(read.body.is_empty());
        let grep = tool_identity(
            "grep",
            &serde_json::json!({"pattern": "parse", "path": "src/"}),
            &p,
        );
        let grep_plain = phi_tui::strip_ansi(&grep.anchor);
        assert!(grep_plain.contains("grep parse in src/"));
        assert!(grep.body.is_empty());
        let ls = tool_identity("ls", &serde_json::json!({"path": "src"}), &p);
        assert!(phi_tui::strip_ansi(&ls.anchor) == "ls src");
        assert!(ls.body.is_empty());
        // bash: single-line command lives in the anchor only (empty body); a
        // multi-line command's truncated first line is the anchor and its full
        // text is the body — the canonical command is shown once either way.
        let bash1 = tool_identity("bash", &serde_json::json!({"cmd": "cargo check"}), &p);
        assert!(phi_tui::strip_ansi(&bash1.anchor).contains("$ cargo check"));
        assert!(bash1.body.is_empty(), "single-line bash must not duplicate the command in the body");
        let bash2 = tool_identity(
            "bash",
            &serde_json::json!({"cmd": "cargo test\ncargo build"}),
            &p,
        );
        assert!(phi_tui::strip_ansi(&bash2.anchor).contains("$ cargo test…"));
        assert!(!bash2.body.is_empty(), "multi-line bash must carry the full command in the body");
        assert_eq!(bash2.body.len(), 2, "{:?}", bash2.body);
        // Unknown (e.g. MCP/extension) tools: small `key=value` args live in the
        // anchor only (empty body — no dup); a long value earns a body row with
        // the full text (anchor keeps the truncated one).
        let small = tool_identity("mcp__x", &serde_json::json!({"q": "hi"}), &p);
        assert_eq!(
            phi_tui::strip_ansi(&small.anchor),
            "mcp__x q=hi",
            "{:?}",
            small.anchor
        );
        assert!(small.body.is_empty(), "small unknown-tool arg must not duplicate in the body");
        let big_cmd = "x".repeat(120);
        let big = tool_identity("mcp__x", &serde_json::json!({"q": big_cmd}), &p);
        assert!(!big.body.is_empty(), "long unknown-tool arg should carry full text in body");
        let joined = big
            .body
            .iter()
            .map(|l| phi_tui::strip_ansi(l))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains(&big_cmd), "{joined}");
        // A bare (non-object) arg is never in the anchor (anchor is just the
        // name), so a non-empty value lives in the body once.
        let bare = tool_identity("mcp__x", &serde_json::json!("hello"), &p);
        assert_eq!(phi_tui::strip_ansi(&bare.anchor), "mcp__x", "{:?}", bare.anchor);
        assert!(!bare.body.is_empty(), "bare-string arg must show its value in the body");
        assert!(
            bare.body
                .iter()
                .any(|l| phi_tui::strip_ansi(l).contains("hello")),
            "{:?}",
            bare.body
        );

        // End-to-end through the card: a read call shows the path once total —
        // in the header rule only — never duplicated by an output body line.
        let ui = test_ui(80, 24);
        let rows = ui.tool_result_rows(
            0,
            "read_file",
            &serde_json::json!({"path": "src/parser.rs"}),
            "// file\n",
            usize::MAX,
            Some("rust"),
            false,
            None,
        );
        let plain: String = rows
            .iter()
            .map(|l| phi_tui::strip_ansi(l))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(plain.matches("src/parser.rs").count(), 1, "{plain}");
        // The output content (here just the file's first line) shows once too.
        assert_eq!(plain.matches("// file").count(), 1, "{plain}");
    }

    // A multi-line bash command is preserved in full under the card header —
    // one body line per command line, none truncated away — while the anchor
    // is the single-line truncation (`…`), so the header stays compact.
    #[test]
    fn bash_body_preserved_multiline() {
        let ui = test_ui(80, 24);
        let rows = ui.tool_result_rows(
            1,
            "bash",
            &serde_json::json!({"cmd": "cargo test\ncargo build --release"}),
            "running 12 tests\n",
            usize::MAX,
            None,
            false,
            None,
        );
        let plain: String = rows
            .iter()
            .map(|l| phi_tui::strip_ansi(l))
            .collect::<Vec<_>>()
            .join("\n");
        // Both command lines survive in the body, not just the first.
        assert!(plain.contains("cargo test"), "{plain}");
        assert!(plain.contains("cargo build --release"), "{plain}");
        // The header anchor is the one-line truncation of the command.
        assert!(plain.contains("$ cargo test…"), "{plain}");
        // Each `$ …` command line appears exactly once (no duplication).
        assert_eq!(plain.matches("cargo test").count(), 2, "{plain}"); // anchor + body line
        // Output preview is folded under the same card, below the command body.
        assert!(plain.contains("running 12 tests"), "{plain}");

        // Single-line bash: the anchor already carries the command, so the body
        // must NOT re-state it (anchor-once — the canonical command appears
        // once total, in the header rule only).
        let ui = test_ui(80, 24);
        let rows = ui.tool_result_rows(
            0,
            "bash",
            &serde_json::json!({"cmd": "cargo check"}),
            "Compiling\n",
            usize::MAX,
            None,
            false,
            None,
        );
        let plain: String = rows
            .iter()
            .map(|l| phi_tui::strip_ansi(l))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(plain.contains("$ cargo check"), "{plain}");
        assert_eq!(plain.matches("cargo check").count(), 1, "{plain}");
        // The output preview still folds under the card.
        assert!(plain.contains("Compiling"), "{plain}");
    }

    // The repl block has no header, so neither a *live* action block (built
    // during an iteration) nor a persisted assistant message re-rendered as
    // prose after the run completes carries a `repl` or `turn` label — the
    // code and output stand on their own.
    #[test]
    fn repl_panel_label_carries_turn_only_for_live_blocks() {
        use tokio_util::sync::CancellationToken;
        let pending_plain = |ui: &Ui| -> String {
            ui.pending
                .iter()
                .map(|l| phi_tui::strip_ansi(l))
                .collect::<Vec<_>>()
                .join("\n")
        };

        // Live block: IterationStart (iteration 0 -> turn 1), then a code exec
        // whose replay commit happens while the run is active.
        let mut ui = test_ui(80, 24);
        ui.running = Some(CancellationToken::new());
        ui.handle_rlm_event(RlmEvent::IterationStart {
            depth: 0,
            iteration: 0,
        });
        ui.handle_rlm_event(RlmEvent::CodeExecStart {
            depth: 0,
            iteration: 0,
            code: "print(1)".to_owned(),
        });
        ui.handle_rlm_event(RlmEvent::CodeExecEnd {
            depth: 0,
            iteration: 0,
            stdout: "1\n".to_owned(),
            stderr: String::new(),
            duration: Duration::from_millis(5),
        });
        let live_plain = pending_plain(&ui);
        assert!(!live_plain.contains("repl"), "{live_plain}");
        assert!(!live_plain.contains("turn"), "{live_plain}");
        assert!(live_plain.contains("print(1)"), "{live_plain}");

        // Persisted assistant message rendered after the run completes (run
        // no longer active): also plain, no `repl`/`turn` label.
        ui.running = None;
        ui.pending.clear();
        ui.push_assistant_message("Here is code:\n\n```repl\nprint(1)\n```\n");
        let replay_plain = pending_plain(&ui);
        assert!(!replay_plain.contains("turn"), "{replay_plain}");
        assert!(replay_plain.contains("print(1)"), "{replay_plain}");
    }

    #[test]
    fn strip_tool_echoes_removes_printed_tool_result() {
        let tools = vec![session::ToolCall {
            name: "grep".to_owned(),
            args: serde_json::json!({"pattern": "exec"}),
            result: "crates/foo.rs:1: hello
"
            .to_owned(),
            error: None,
            duration_secs: 0.1,
            depth: 0,
        }];
        // stdout == tool result -> fully stripped (empty residual)
        assert_eq!(
            strip_tool_echoes(
                "crates/foo.rs:1: hello
",
                &tools,
                0
            ),
            ""
        );
        // stdout != tool result -> unchanged
        assert_eq!(
            strip_tool_echoes(
                "different output
",
                &tools,
                0
            ),
            "different output"
        );
        // no tools -> unchanged
        assert_eq!(
            strip_tool_echoes(
                "crates/foo.rs:1: hello
",
                &[],
                0
            ),
            "crates/foo.rs:1: hello"
        );
        // wrong depth -> unchanged
        assert_eq!(
            strip_tool_echoes(
                "crates/foo.rs:1: hello
",
                &tools,
                1
            ),
            "crates/foo.rs:1: hello"
        );
    }

    #[test]
    fn strip_tool_echoes_removes_multiple_tool_results() {
        let tools = vec![
            session::ToolCall {
                name: "grep".to_owned(),
                args: serde_json::json!({}),
                result: "match1
"
                .to_owned(),
                error: None,
                duration_secs: 0.1,
                depth: 0,
            },
            session::ToolCall {
                name: "find".to_owned(),
                args: serde_json::json!({}),
                result: "file1
"
                .to_owned(),
                error: None,
                duration_secs: 0.1,
                depth: 0,
            },
        ];
        // stdout == concatenation of both tool results -> fully stripped
        assert_eq!(
            strip_tool_echoes(
                "match1
file1
",
                &tools,
                0
            ),
            ""
        );
        // stdout only matches one -> the other line remains
        assert_eq!(
            strip_tool_echoes(
                "match1
", &tools, 0
            ),
            ""
        );
    }

    #[test]
    fn strip_tool_echoes_preserves_non_tool_lines() {
        let tools = vec![session::ToolCall {
            name: "grep".to_owned(),
            args: serde_json::json!({}),
            result: "match1
"
            .to_owned(),
            error: None,
            duration_secs: 0.1,
            depth: 0,
        }];
        // Mixed: tool echo + extra print -> only the extra line remains
        assert_eq!(
            strip_tool_echoes(
                "match1
done
",
                &tools,
                0
            ),
            "done"
        );
        // Extra line before tool echo
        assert_eq!(
            strip_tool_echoes(
                "starting
match1
",
                &tools,
                0
            ),
            "starting"
        );
    }

    #[test]
    fn repl_code_preview_uses_input_section_not_inner_fence() {
        let ui = test_ui(80, 24);
        let plain = ui
            .repl_code_lines(0, "print(1)", usize::MAX)
            .into_iter()
            .map(|line| phi_tui::strip_ansi(&line))
            .collect::<Vec<_>>()
            .join(
                "
",
            );
        // Input label removed for minimalist TUI
        assert!(!plain.contains("```"), "{plain}");
    }

    #[test]
    fn chrome_lines_editor_mode_has_no_panel_box() {
        let mut ui = test_ui(80, 24);
        ui.editor.set_text("hi");

        let (lines, cursor) = ui.chrome_lines();
        let plain: Vec<String> = lines.iter().map(|line| phi_tui::strip_ansi(line)).collect();

        // No box-drawing characters: the forgecode-style chrome has no
        // ┌/│/└ panel borders.
        let box_lines: Vec<&String> = plain
            .iter()
            .filter(|line| line.starts_with('\u{250c}') || line.starts_with('\u{2502}') || line.starts_with('\u{2514}'))
            .collect();
        assert!(box_lines.is_empty(), "box lines should be gone: {plain:?}");

        // Line 0 is the separator, line 1 is the header, line 2 is the
        // chevron prompt with "hi".
        assert!(plain.len() >= 3, "{plain:?}");
        // Separator line is all box-drawing chars.
        assert!(plain[0].chars().all(|c| c == '\u{2500}'), "separator line: {plain:?}");
        // Header contains the agent symbol text "PHI"
        assert!(plain[1].contains("PHI"), "header should contain agent: {plain:?}");
        // Input line contains the editor text
        assert!(plain[2].contains("hi"), "input line should contain 'hi': {plain:?}");

        // Cursor is present (the editor has text).
        assert!(cursor.is_some(), "{plain:?}");
    }

    #[test]
    fn chrome_lines_editor_mode_header_shows_dir_and_rprompt() {
        let mut ui = test_ui(80, 24);
        ui.editor.set_text("hi");
        ui.cwd = std::path::PathBuf::from("/tmp/phi");
        ui.branch = Some("main".to_owned());
        ui.branch_at = std::time::Instant::now();

        let (lines, _cursor) = ui.chrome_lines();
        let plain: Vec<String> = lines.iter().map(|line| phi_tui::strip_ansi(line)).collect();

        // The header line (line 1, after separator) should contain:
        //  - the directory name "phi" (last path component, not full path)
        //  - the branch "main"
        //  - the agent label "PHI"
        //  - the model id (stripped of provider prefix)
        let header = &plain[1];
        assert!(header.contains("phi"), "header should contain dir name: {header:?}");
        assert!(header.contains("main"), "header should contain branch: {header:?}");
        assert!(header.contains("PHI"), "header should contain agent: {header:?}");
        assert!(!header.contains("/tmp/phi"), "header should not contain full path: {header:?}");

        // No footer line in non-framed mode.
        assert!(
            !plain.iter().any(|line| line.contains("thinking:")),
            "no footer in non-framed mode: {plain:?}"
        );
    }

    #[test]
    fn chrome_lines_ascii_fallback_when_nerd_fonts_disabled() {
        unsafe { std::env::set_var("PHI_NO_NERD_FONTS", "1"); };
        let mut ui = test_ui(80, 24);
        ui.editor.set_text("hi");

        let (lines, _cursor) = ui.chrome_lines();
        let plain: Vec<String> = lines.iter().map(|line| phi_tui::strip_ansi(line)).collect();

        // In ASCII fallback mode the chevron is ">" and there are no Nerd
        // Font PUA codepoints in the output.
        assert!(plain[2].starts_with(">"), "ASCII chevron: {plain:?}");
        assert!(!plain.iter().any(|l| l.contains('\u{ea83}')), "no nerd folder icon: {plain:?}");
        assert!(!plain.iter().any(|l| l.contains('\u{f013e}')), "no nerd chevron: {plain:?}");
        unsafe { std::env::remove_var("PHI_NO_NERD_FONTS"); };
    }

    #[test]
    fn chrome_lines_effort_label_width_thresholds() {
        assert_eq!(effort_label(ThinkingLevel::Medium, 80), Some("MED".to_string()));
        assert_eq!(effort_label(ThinkingLevel::Medium, 100), Some("MEDIUM".to_string()));
        assert_eq!(effort_label(ThinkingLevel::Off, 80), None);
        assert_eq!(effort_label(ThinkingLevel::High, 50), Some("HIG".to_string()));
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
    fn streamed_paragraphs_commit_as_one_assistant_message_card() {
        let mut ui = test_ui(80, 24);
        ui.running = Some(CancellationToken::new());
        ui.handle_rlm_event(RlmEvent::IterationStart {
            depth: 0,
            iteration: 0,
        });
        ui.handle_rlm_event(RlmEvent::RootDelta {
            depth: 0,
            iteration: 0,
            delta: "para one\n\n".to_owned(),
        });

        // Completed streamed paragraphs are live-only; they must not be
        // finalized into retained Assistant cards before the JSONL assistant
        // message exists.
        assert!(
            ui.pending.iter().all(|line| !line.contains("Assistant")),
            "{:?}",
            ui.pending
        );

        ui.handle_rlm_event(RlmEvent::RootDelta {
            depth: 0,
            iteration: 0,
            delta: "para two".to_owned(),
        });
        ui.handle_rlm_event(RlmEvent::RootText {
            depth: 0,
            iteration: 0,
            text: "para one\n\npara two".to_owned(),
        });

        let assistant_cards = ui
            .pending
            .iter()
            .filter(|line| line.contains("Assistant"))
            .count();
        assert_eq!(assistant_cards, 1, "{:?}", ui.pending);
    }

    #[test]
    fn streamed_prose_around_block_is_not_recommitted_by_root_text() {
        // Regression: prose before an executed block was committed once by
        // `CodeExecStart` (to sit above the block) and a second time by
        // `RootText` re-splitting the full message, leaving the earlier prose
        // re-rendered after the code block.
        let mut ui = test_ui(80, 24);
        ui.running = Some(CancellationToken::new());
        ui.handle_rlm_event(RlmEvent::IterationStart {
            depth: 0,
            iteration: 0,
        });
        // The model streams: intro prose, a repl block, then trailing prose.
        ui.handle_rlm_event(RlmEvent::RootDelta {
            depth: 0,
            iteration: 0,
            delta: "intro line\n\n```repl\nprint(1)\n```".to_owned(),
        });
        ui.handle_rlm_event(RlmEvent::CodeExecStart {
            depth: 0,
            iteration: 0,
            code: "print(1)".to_owned(),
        });
        ui.handle_rlm_event(RlmEvent::CodeExecEnd {
            depth: 0,
            iteration: 0,
            stdout: "1\n".to_owned(),
            stderr: String::new(),
            duration: Duration::from_millis(5),
        });
        ui.handle_rlm_event(RlmEvent::RootDelta {
            depth: 0,
            iteration: 0,
            delta: "\n\ntrailing line".to_owned(),
        });
        ui.handle_rlm_event(RlmEvent::RootText {
            depth: 0,
            iteration: 0,
            text: "intro line\n\n```repl\nprint(1)\n```\n\ntrailing line".to_owned(),
        });

        let intro_cards = ui
            .pending
            .iter()
            .filter(|line| line.contains("intro line"))
            .count();
        assert_eq!(intro_cards, 1, "intro prose duplicated: {:?}", ui.pending);
        let trailing_cards = ui
            .pending
            .iter()
            .filter(|line| line.contains("trailing line"))
            .count();
        assert_eq!(trailing_cards, 1, "{:?}", ui.pending);
    }

    #[test]
    fn replay_branch_preserves_depth_one_repl_activity() {
        let mut ui = test_ui(100, 30);
        ui.store.append(session::Role::User, "q1").unwrap();

        ui.handle_rlm_event(RlmEvent::CodeExecStart {
            depth: 0,
            iteration: 0,
            code: "print(llm_query('hello'))\nprint(bash('echo nested'))".to_owned(),
        });
        ui.handle_rlm_event(RlmEvent::SubcallStart {
            depth: 1,
            model: "sub-model".to_owned(),
            prompt_preview: "hello".to_owned(),
        });
        ui.handle_rlm_event(RlmEvent::SubcallEnd {
            depth: 1,
            model: "sub-model".to_owned(),
            result_preview: Some("world".to_owned()),
            duration: Duration::from_millis(200),
            error: None,
        });
        ui.handle_rlm_event(RlmEvent::ToolStart {
            depth: 1,
            call_id: 7,
            name: "bash".to_owned(),
            args: serde_json::json!({"cmd": "echo nested"}),
        });
        ui.handle_rlm_event(RlmEvent::ToolEnd {
            depth: 1,
            call_id: 7,
            name: "bash".to_owned(),
            result: "nested\n".to_owned(),
            error: None,
            duration: Duration::from_millis(100),
        });
        ui.handle_rlm_event(RlmEvent::CodeExecEnd {
            depth: 0,
            iteration: 0,
            stdout: "done\n".to_owned(),
            stderr: String::new(),
            duration: Duration::from_millis(300),
        });

        let checkpoint = ui
            .store
            .branch_items()
            .into_iter()
            .find_map(|item| match item {
                session::BranchItem::Repl(repl) => Some(repl.clone()),
                session::BranchItem::Message(_) => None,
            })
            .expect("checkpointed repl");
        assert_eq!(checkpoint.subcalls.len(), 1);
        assert_eq!(checkpoint.subcalls[0].depth, 1);
        assert_eq!(checkpoint.tools.len(), 1);
        assert_eq!(checkpoint.tools[0].depth, 1);
        assert_eq!(checkpoint.tools[0].args["cmd"], "echo nested");

        ui.replay_branch();
        let plain = ui
            .pending
            .iter()
            .map(|line| phi_tui::strip_ansi(line))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(plain.contains("sub-model"), "{plain}");
        assert!(plain.contains("\"hello\""), "{plain}");
        assert!(plain.contains("world"), "{plain}");
        assert!(plain.contains("echo nested"), "{plain}");
        assert!(plain.contains("nested"), "{plain}");
    }

    #[test]
    fn replay_branch_uses_assistant_display_thinking_for_transcript_toggle() {
        let mut ui = test_ui(80, 24);
        ui.store.append(session::Role::User, "q1").unwrap();
        ui.store
            .append_with_usage_and_display(
                session::Role::Assistant,
                "done",
                assistant_display_text("done", "secret thought"),
                None,
            )
            .unwrap();

        ui.replay_branch();
        let collapsed = ui.pending.join("\n");
        assert!(!collapsed.contains("secret thought"), "{collapsed}");

        ui.toggle_thinking();
        let expanded = ui.pending.join("\n");
        assert!(expanded.contains("secret thought"), "{expanded}");
        assert!(
            !expanded.contains("thinking expanded throughout transcript"),
            "{expanded}"
        );

        ui.toggle_thinking();
        let recollapsed = ui.pending.join("\n");
        assert!(
            !recollapsed.contains("thinking collapsed throughout transcript"),
            "{recollapsed}"
        );
    }

    #[test]
    fn toggle_tools_replays_existing_transcript_with_expanded_output() {
        let mut ui = test_ui(80, 24);
        ui.store.append(session::Role::User, "q1").unwrap();
        ui.store
            .append_repl(session::ReplRecord {
                code: "print(bash('echo hi'))".to_owned(),
                stdout: "line1\nline2\nline3\nline4\nline5\nline6\n".to_owned(),
                stderr: String::new(),
                duration_secs: 0.1,
                kind: session::ReplKind::Shell,
                tools: Vec::new(),
                subcalls: Vec::new(),
            })
            .unwrap();

        ui.replay_branch();
        let collapsed = ui.pending.join("\n");
        assert!(collapsed.contains("… (1 more lines)"), "{collapsed}");

        ui.toggle_tools();
        let expanded = ui.pending.join("\n");
        assert!(expanded.contains("line6"), "{expanded}");
        assert!(!expanded.contains("… (1 more lines)"), "{expanded}");
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
        let popup = complete("/", &models(), &[]).unwrap();
        assert_eq!(popup.items.len(), COMMANDS.len());
        assert!(popup.kind == PopupKind::Command);
        assert_eq!(popup.prefix, "/");
    }

    #[test]
    fn command_prefix_filters() {
        // Fuzzy: "/mo" matches "model" (best) and "submodel".
        let popup = complete("/mo", &models(), &[]).unwrap();
        assert_eq!(popup.items.len(), 2);
        assert_eq!(popup.items[0].value, "model");
        assert_eq!(popup.items[1].value, "submodel");
        assert!(complete("/zzz", &models(), &[]).is_none());
        assert!(complete("hello", &models(), &[]).is_none());
    }

    #[test]
    fn model_argument_completion() {
        let popup = complete("/model ", &models(), &[]).unwrap();
        assert_eq!(popup.items.len(), 2);
        assert!(popup.kind == PopupKind::Argument);
        let popup = complete("/model opus", &models(), &[]).unwrap();
        assert_eq!(popup.items.len(), 1);
        assert_eq!(popup.items[0].value, "anthropic/claude-opus-4-7");
        assert_eq!(popup.prefix, "opus");
    }

    #[test]
    fn provider_argument_completion() {
        let popup = complete("/login a", &models(), &[]).unwrap();
        assert_eq!(popup.items.len(), 2); // both contain 'a'
        let popup = complete("/login anth", &models(), &[]).unwrap();
        assert_eq!(popup.items[0].value, "anthropic");
    }

    #[test]
    fn no_argument_completion_for_other_commands() {
        assert!(complete("/new ", &models(), &[]).is_none());
        assert!(complete("/model two words", &models(), &[]).is_none());
    }
}
