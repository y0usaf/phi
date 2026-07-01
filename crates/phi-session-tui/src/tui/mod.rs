//! in-TUI via /login, mirroring ref/phi: the OAuth flow runs in the
//! background, the URL lands in scrollback, and the editor doubles as the
//! paste-the-redirect-URL input (racing the localhost callback server).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use phi_ext::AppRuntime;

use anyhow::Result;
use phi_ai::{
    AuthStorage, LoginCallbacks, LoginMethod, Model, OAuthProviderId, Registry, ThinkingLevel,
    Usage,
};
use phi_rlm::{
    ContextPayload, ProviderLm, ReplResult, RestoredExchange, RestoredRepl, Rlm, RlmCompletion,
    RlmEvent, RlmSession, StopCause,
};
use phi_tui::crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use phi_tui::{
    Editor, EditorAction, LiveCursor, RawModeGuard, Selector, SelectorItem, SelectorOutcome,
    Terminal, TerminalColors, detect_terminal_colors, equalizer_glyphs, fill_bg, fuzzy_filter,
    gradient_text_animated, highlight_code, highlight_diff_line, highlight_output, render_markdown,
    shimmer_ansi, strip_ansi, visible_width, wrap_ansi, wrap_ansi_locate,
};

use phi_session::files::FileSearcher;
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
mod chat;
mod composition;
mod editor;
mod events;
mod navigation;
/// Truecolor ANSI escapes for the inline TUI's transcript/chrome rendering,
/// derived from the host terminal's reported colors when available. Mirrors
/// opencode's `generateSystem` mapping. `default_const()` returns the previous
/// hardcoded values as the fallback. Basic ANSI colors (`RESET`, `BOLD`, etc.)
/// stay as module-level `&str` since they already reference the terminal palette.
mod palette;
mod popup;
mod render;
mod symbols;
mod tool_display;
mod toolbar;
mod visual_context;

use events::LoginState;
#[cfg(test)]
use events::python_quote;

pub use palette::InlinePalette;
pub(crate) use render::{
    OpenSubcallRow, OpenToolRow, ReplBlockData, TOOL_RESULT_LOG_BUDGET, TranscriptBlock,
    TranscriptEntry,
};
pub(crate) use tool_display::{format_model_pricing, format_tokens, tool_identity, tool_title};
pub(super) use visual_context::VisualContext;

use popup::*;
use render::*;
use symbols::*;
use tool_display::*;

pub(super) enum Cmd {
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

pub(super) enum Done {
    Run(RlmCompletion),
    Shell {
        code: String,
        record: bool,
        result: Result<ReplResult, String>,
    },
    Restored(Result<(), String>),
}

pub(super) enum LoginMsg {
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
    pub tools: phi_session::config::ToolsSettings,
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

pub(super) struct PendingHostModal {
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
    /// Terminal colors probed by the host (OSC 10/11/4). When `Some`, the
    /// session TUI derives its palette from these instead of probing the
    /// terminal itself — the mux already owns the raw terminal and has
    /// already probed, so the embedded session reuses that result. When
    /// `None` (standalone mode), the session probes its own terminal.
    pub terminal_colors: Option<TerminalColors>,
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
            terminal_colors: None,
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

/// Install a panic hook that logs the panic info to a file before the
/// default hook prints to stderr. This ensures a diagnostic trail even
/// when the terminal is in raw mode (where stderr output may be garbled).
fn install_panic_log_hook() {
    std::panic::set_hook(Box::new(|info| {
        let Some(dirs) = directories::ProjectDirs::from("", "", "phi") else {
            return;
        };
        let path = dirs.data_dir().join("tui-panic.log");
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            use std::io::Write;
            let _ = writeln!(file, "[{:?}] panic: {info}", std::time::SystemTime::now());
        }
    }));
}

pub async fn run(setup: TuiSetup, mut io: SessionIo) -> Result<()> {
    install_panic_log_hook();
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
        let mut session = RlmSession::new(rlm, payload);
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
    let mut term = Terminal::new(writer, width, height);
    term.set_embedded(io.embedded);
    let mut ui = Ui::new(
        term,
        ui_setup,
        login_tx,
        io.cwd.clone(),
        io.host_modal_tx.take(),
        runtime,
    );
    // Always populate the visual context from the extension runtime —
    // the runtime has extensions (spinners, thinking expressions, text
    // effects) registered in both standalone and embedded modes.
    ui.visual = VisualContext::from_runtime(Some(&ui.runtime));

    // Derive the inline TUI palette from the terminal's colors so the
    // session content matches the mux chrome. In embedded mode the mux has
    // already probed the terminal (it owns the raw tty), so we reuse its
    // result via `io.terminal_colors` instead of probing again. In
    // standalone mode we probe here. Falls back to the built-in hardcoded
    // palette when no colors are available.
    let colors = if let Some(tc) = io.terminal_colors.take() {
        Some(tc)
    } else if !io.embedded {
        detect_terminal_colors(Duration::from_millis(150))
    } else {
        None
    };
    if let Some(colors) = colors {
        ui.palette = InlinePalette::from_terminal_colors(&colors);
    }
    // Resume when the session has any message entries, regardless of the
    // leaf pointer. A session navigated to root (leaf None) still has
    // entries and must replay its branch; only a truly empty (new) session
    // shows the greeting.
    let restoring = ui.store.has_entries();
    if restoring {
        ui.replay_branch();
        ui.restore_rlm(&cmd_tx);
    }
    let mut input = match io.event_rx.take() {
        Some(rx) => InputSource::Forwarded(rx),
        None => InputSource::Tty(EventStream::new()),
    };
    let mut ticker = tokio::time::interval(Duration::from_millis(40));
    // Coalesce redraws to the same 40ms cadence as the spinner tick: bursts
    // of stream deltas collapse into one frame instead of repainting per
    // SSE chunk. Terminals bottleneck at ~25fps anyway.
    const FRAME: Duration = Duration::from_millis(40);
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
    if let Err(e) = ui.draw() {
        eprintln!("phi: render error: {e}");
    }
    // Fire-and-forget TuiReady so a slow/blocked startup handler can't freeze
    // the UI before the event loop starts. The dispatch primitive bounds the
    // handler; the event loop begins immediately.
    {
        let rt = Arc::clone(&ui.runtime);
        tokio::spawn(async move {
            let _ = rt.on_tui_ready().await;
        });
    }

    loop {
        let quit = tokio::select! {
            maybe = input.next() => match maybe {
                #[cfg(unix)]
                Some(Ok(Event::Key(key)))
                    if guard.is_some()
                        && !io.embedded
                        && key.kind != KeyEventKind::Release
                        && key.code == KeyCode::Char('q')
                        && key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    let _ = ui.term.finish();
                    if let Some(guard) = &guard {
                        guard.suspend();
                    }
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
                ui.refresh_open_activity_rows();
                for msg in ui.runtime.drain_notifications() {
                    ui.pending.push(TranscriptEntry::RawLines(vec![msg]));
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
                Err(e) => ui.pending.push(TranscriptEntry::RawLines(vec![ui.err(&e)])),
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
                Err(e) => ui.pending.push(TranscriptEntry::RawLines(vec![ui.err(&e)])),
            }
        }
        dirty = true;
        if last_draw.elapsed() >= FRAME {
            if let Err(e) = ui.draw() {
                eprintln!("phi: render error: {e}");
            }
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
    if let Err(e) = ui.term.finish() {
        eprintln!("phi: terminal finish error: {e}");
    }
    drop(guard);
    phi_tui::drain_input();
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

struct Ui {
    pub(super) term: Terminal<Box<dyn std::io::Write + Send>>,
    pub(super) editor: Editor,
    pub(super) visual: VisualContext,
    /// When the current run's equalizer animation started (wall-clock).
    /// Reset each time `running` transitions to `Some`.
    pub(super) anim_start: std::time::Instant,
    pub(super) palette: InlinePalette,
    /// Cancel token of the in-flight turn, if any.
    pub(super) running: Option<CancellationToken>,
    /// In-flight /login flow, if any. The editor feeds it pasted codes.
    pub(super) login: Option<LoginState>,
    /// Slash command autocomplete, shown below the editor.
    pub(super) popup: Option<Popup>,
    /// Active full-width selector (model/resume/fork/tree); replaces the
    /// editor area and captures all keys until confirmed or cancelled.
    pub(super) selector: Option<(SelectorKind, Selector)>,
    /// On-disk session store recording the conversation tree.
    pub(super) store: SessionStore,
    /// Time of the last esc.  When idle with an empty editor, a second esc
    /// within 500ms opens the session tree (pi's double-escape).  When a run
    /// is in flight, a second esc within 500ms cancels the run.
    pub(super) last_esc: Option<std::time::Instant>,
    /// Worktree file search backing `@` completion.
    pub(super) files: FileSearcher,
    pub(super) cwd: std::path::PathBuf,
    /// Set by ctrl+g; the run loop opens $VISUAL/$EDITOR on the buffer.
    pub(super) external_edit: bool,
    /// ctrl+o: show full code/output instead of previews across the
    /// transcript by rebuilding retained history, pi-style.
    pub(super) expand_tools: bool,
    /// ctrl+t: show provider thinking across the transcript by rebuilding
    /// retained history, pi-style.
    pub(super) expand_thinking: bool,
    /// How to render tool cards and REPL blocks: tool_max_lines (0 = header only),
    /// repl_max_lines (0 = header only), tool_padding, repl_padding, user_padding, gap.
    /// Set from config.lua `tools = { tool_max_lines = 0, repl_max_lines = 0, tool_padding = 0, repl_padding = 0, user_padding = 0, gap = 0 }`.
    pub(super) tools: phi_session::config::ToolsSettings,
    /// Follow-up prompts queued while a turn is in flight; sent FIFO as
    /// turns complete.
    pub(super) queued: Vec<String>,
    /// How messages submitted mid-run get dispatched (queue/interleave/interrupt).
    /// Time of the last ctrl-c; a second one within 500ms quits.
    pub(super) last_ctrl_c: Option<std::time::Instant>,
    /// Lines staged by event handlers since the last draw; each draw folds
    /// them into a retained transcript block before rendering.
    pub(super) pending: Vec<TranscriptEntry>,
    /// Retained transcript renderer/commit seam, restored from the archived
    /// Phi transcript-block model.
    pub(super) transcript: Transcript,
    /// Finalized transcript blocks retained across frames.
    pub(super) transcript_entries: Vec<TranscriptEntry>,
    /// Cached rendered transcript lines keyed by "tx:{i}". Rebuilt only when
    /// entries change or the terminal width / expand toggles change — not
    /// on every frame. This is the key performance fix: without it, draw()
    /// re-renders every entry (markdown parsing, syntax highlighting, REPL
    /// block construction) on every keystroke.
    pub(super) transcript_cache: Vec<(String, Vec<String>)>,
    /// Terminal width when the cache was built; mismatch invalidates it.
    pub(super) transcript_cache_width: usize,
    /// (expand_tools, expand_thinking) when the cache was built.
    pub(super) transcript_cache_expanded: (bool, bool),
    /// Rows of the in-flight depth-0 ```repl``` block (with everything nested in
    /// it), shown live and then committed to scrollback verbatim.
    pub(super) block: Vec<String>,
    pub(super) block_active: bool,
    pub(super) block_errored: bool,
    /// Code of the in-flight depth-0 agent ```repl``` block, stashed at
    /// `CodeExecStart` and persisted to the session store at `CodeExecEnd`.
    pub(super) live_code: Option<String>,
    /// Tool calls observed inside the in-flight depth-0 ```repl``` block; args
    /// stashed at `ToolStart`, completed at `ToolEnd`, persisted with the
    /// block at `CodeExecEnd` so resume re-renders the same tool cards.
    pub(super) live_tools: Vec<session::ToolCall>,
    /// Args of tools still executing inside the buffered depth-0 REPL block,
    /// keyed by (RLM event depth, engine call_id). Child REPLs have their own
    /// call-id spaces, so depth is part of the key.
    pub(super) open_tool_args: std::collections::HashMap<(usize, u64), serde_json::Value>,
    /// Live tool status rows inside the buffered block.
    pub(super) open_tool_rows: std::collections::HashMap<(usize, u64), OpenToolRow>,
    /// Sub-LLM calls observed inside the in-flight depth-0 ```repl``` block.
    pub(super) live_subcalls: Vec<session::SubcallRecord>,
    /// Live subcall status rows inside the buffered block.
    pub(super) open_subcall_rows: std::collections::VecDeque<OpenSubcallRow>,
    /// Prose flushed while a block is active; committed after the block so
    /// scrollback stays chronological.
    pub(super) after_block: Vec<String>,
    /// Whether the last scrollback line was nested activity; a blank line is
    /// inserted before a depth-0 action that follows prose.
    pub(super) last_activity: bool,
    /// Streamed root-model text for the current iteration, minus the
    /// paragraphs/fences already flushed to scrollback.
    pub(super) stream: String,
    /// Streamed provider thinking for the current iteration.
    pub(super) thinking: String,
    /// Provider thinking captured across the whole in-flight turn so replay
    /// can re-render historical thinking blocks under the current toggle.
    pub(super) thinking_log: String,
    /// Whether any `RootDelta` arrived this iteration; when false the model
    /// did not stream and `RootText` carries the only copy of the text.
    pub(super) streamed: bool,
    pub(super) status: String,
    pub(super) turn: usize,
    pub(super) model_label: String,
    pub(super) storage: Arc<AuthStorage>,
    pub(super) registry: Arc<Registry>,
    pub(super) root_lm: Arc<ProviderLm>,
    pub(super) sub_lm: Arc<ProviderLm>,
    pub(super) runtime: Arc<AppRuntime>,
    /// Qualified `provider/id` names offered by /model completion.
    pub(super) model_names: Vec<String>,
    pub(super) login_tx: mpsc::UnboundedSender<LoginMsg>,
    pub(super) host_modal_tx: Option<mpsc::UnboundedSender<HostModalRequest>>,
    pub(super) pending_host_modal: Option<PendingHostModal>,
    /// Usage of the most recent completed run (footer stats).
    pub(super) usage: Usage,
    /// Cached git branch for the footer, refreshed on a short TTL.
    pub(super) branch: Option<String>,
    pub(super) branch_at: std::time::Instant,
}

/// Current git branch of `cwd` (or a short detached-HEAD hash), following a
/// worktree `gitdir:` redirect.
/// Abbreviate `$HOME` to `~` for display.
pub(super) fn display_path(path: &std::path::Path) -> String {
    let s = path.display().to_string();
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && s.starts_with(&home) => format!("~{}", &s[home.len()..]),
        _ => s,
    }
}

pub(super) fn model_label(root: &Model, sub: &Model) -> String {
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
            tools,
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
            visual: VisualContext::default(),
            anim_start: std::time::Instant::now(),
            palette: InlinePalette::default_const(),
            running: None,
            login: None,
            popup: None,
            selector: None,
            store,
            last_esc: None,
            files: FileSearcher::new(cwd.clone()),
            cwd,
            external_edit: false,
            expand_tools: false,
            expand_thinking: false,
            tools,
            queued: Vec::new(),
            last_ctrl_c: None,
            pending: Vec::new(),
            transcript: Transcript::new(),
            transcript_entries: Vec::new(),
            transcript_cache: Vec::new(),
            transcript_cache_width: 0,
            transcript_cache_expanded: (false, false),
            block: Vec::new(),
            block_active: false,
            block_errored: false,
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

    pub(super) fn err(&self, msg: &impl std::fmt::Display) -> String {
        format!("{}error:{RESET} {msg}", &self.palette.error_fg)
    }

    pub(super) fn warn(&self, msg: &impl std::fmt::Display) -> String {
        format!("{}{msg}{RESET}", &self.palette.warning_fg)
    }

    fn greet(&mut self, missing_credentials: &[String]) {
        self.pending.push(TranscriptEntry::RawLines(vec![format!(
            "{BOLD}phi{RESET} {DIM}· {} · /help for commands{RESET}",
            self.model_label
        )]));
        for provider in missing_credentials {
            self.pending.push(TranscriptEntry::RawLines(vec![format!(
                "{}no credentials for '{provider}'{RESET} {DIM}— type /login to authenticate{RESET}",
                &self.palette.warning_fg
            )]));
        }
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
            phi_ext::RuntimeBuilder::new()
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
                tools: phi_session::config::ToolsSettings {
                    tool_max_lines: 5,
                    repl_max_lines: 8,
                    tool_padding: 1,
                    repl_padding: 1,
                    user_padding: 1,
                    gap: 1,
                    tool_summary: false,
                },
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

    fn pending_lines(ui: &mut Ui) -> Vec<String> {
        let entries = ui.pending.clone();
        entries
            .into_iter()
            .flat_map(|e| ui.render_entry(&e))
            .collect()
    }

    #[test]
    fn subcall_title_flattens_multiline_preview() {
        let title = subcall_title(
            "model-x",
            "first line\n\nsecond\tline",
            &InlinePalette::default_const(),
        );
        assert!(title.contains("\"first line second line\""), "{title}");
        assert!(!title.contains('\n'), "{title}");
        assert!(!title.contains('\t'), "{title}");
    }

    // Flat form: a bare header line (no trailing `─…─` rule) plus a body
    // indented under a 2-space result gutter. No glyph — the header is just
    // the title at the nesting indent, the body is plain indented text.
    #[test]
    fn render_flat_is_a_bare_header_with_indented_body() {
        let lines = render_flat("read foo.rs", 1, &["body one".to_owned()]);
        let plain: Vec<String> = lines.iter().map(|l| phi_tui::strip_ansi(l)).collect();
        // Header at depth 1: 2-space indent + title, no glyph.
        assert_eq!(plain[0], "  read foo.rs", "{plain:?}");
        assert!(
            !plain[0].contains('⎿'),
            "header must not carry ⎿: {plain:?}"
        );
        assert!(!plain[0].contains('─'), "no trailing rule fill: {plain:?}");
        // Result body: indent(1)=2 + result gutter 2 = 4 cells, then content.
        assert_eq!(plain[1], "    body one", "{plain:?}");
    }

    // Every body row shares the same plain 2-space result gutter — no glyph
    // on any row, so multi-line results stay flat and uniform.
    #[test]
    fn render_flat_shows_single_crop_for_multiline_body() {
        let lines = render_flat(
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
        assert_eq!(
            plain.iter().map(|l| l.matches('⎿').count()).sum::<usize>(),
            0,
            "{plain:?}"
        );
        // Every non-empty body row: indent(1)=2 + result gutter 2 = 4 cells.
        assert_eq!(plain[1], "    line one", "{plain:?}");
        assert_eq!(plain[2], "    line two", "{plain:?}");
        // Empty body line collapses to the depth indent (2 at depth 1).
        assert_eq!(plain[3], "  ", "{plain:?}");
        // A non-empty row after a blank keeps the same gutter.
        assert_eq!(plain[4], "    line four", "{plain:?}");
    }

    // forge_title produces a bare header — just the indent + title, no
    // timestamp bracket, no glyph.
    #[test]
    fn forge_title_is_bare_header() {
        let header = phi_tui::strip_ansi(&forge_title(0, "read x"));
        assert_eq!(header, "read x", "{header}");
        assert!(!header.contains('['), "{header}");
        assert!(!header.contains("●"), "{header}");
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
        assert_eq!(phi_tui::visible_width(&indent(3)), 6);
        // Result gutter is 2 cells at every depth (indent + 2).
        for depth in 0..=4 {
            assert_eq!(
                result_prefix_width(depth),
                indent_width(depth) + 2,
                "depth {depth}"
            );
            assert_eq!(
                phi_tui::visible_width(&result_prefix(depth)),
                result_prefix_width(depth),
                "depth {depth}"
            );
        }
        // card_body_width == repl_body_width - result_prefix_width.
        let ui = test_ui(80, 24);
        for depth in 0..=4 {
            assert_eq!(
                ui.card_body_width(depth),
                ui.repl_body_width()
                    .saturating_sub(result_prefix_width(depth))
                    .max(1),
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
        );
        let panel = ui.panel_width();
        for line in &lines {
            assert!(
                phi_tui::visible_width(line) <= panel,
                "{} > {} => {}",
                phi_tui::visible_width(line),
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
        // Pi-style boxed block: 1-space left pad (Box(1,1)). "read" starts
        // after 1 space. No depth indentation — the box IS the structure.
        assert!(plain.lines().any(|l| l.starts_with(" read")), "{plain}");
        assert!(!plain.lines().any(|l| l.contains('⎿')), "{plain}");
    }

    // Boxed form for a repl block: no header — code, exec bar, and stdout
    // are wrapped in a pi-style Box(1,1,bgFn). The box provides the visual
    // structure; no `output` label, no `├` separator, no ASCII border chrome.
    #[test]
    fn repl_panel_splits_input_and_output_sections() {
        let mut ui = test_ui(80, 24);
        ui.begin_block();
        ui.push_repl_block(0, "print(1)", usize::MAX);
        ui.push_bar(0, "exec 0.1s".to_owned());
        ui.push_preview(0, "1\n", usize::MAX);

        let lines = render_repl_block(
            &ui.block,
            &ui.palette.tool_success_bg,
            ui.panel_width(),
            ui.tools.repl_padding,
        );
        let plain = lines
            .iter()
            .map(|line| phi_tui::strip_ansi(line))
            .collect::<Vec<_>>()
            .join("\n");

        // The repl block now has a bold "repl" anchor header.
        assert!(plain.contains("repl"), "{plain}");
        // Code and exec bar are present as rows inside the box.
        assert!(plain.contains("print(1)"), "{plain}");
        assert!(plain.contains("exec 0.1s"), "{plain}");
        // The stdout value ("1") is present as a row, with no "output" label.
        assert!(plain.lines().any(|l| l.trim() == "1"), "{plain}");
        assert!(!plain.contains("output"), "{plain}");
        assert!(!plain.contains("Output"), "{plain}");
        // No ASCII box-drawing chrome — the box is a full-width bg fill, not
        // a bordered frame.
        assert!(!plain.contains('┌'), "{plain}");
        assert!(!plain.contains('├'), "{plain}");
        assert!(!plain.contains('└'), "{plain}");
        // The block has top and bottom bg padding rows (blank lines from the
        // Box(1,1) padding) — at least 2 more rows than the 3 content rows.
        assert!(lines.len() >= 7, "{lines:?}");
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
        assert!(
            bash1.body.is_empty(),
            "single-line bash must not duplicate the command in the body"
        );
        let bash2 = tool_identity(
            "bash",
            &serde_json::json!({"cmd": "cargo test\ncargo build"}),
            &p,
        );
        assert!(phi_tui::strip_ansi(&bash2.anchor).contains("$ cargo test…"));
        assert!(
            !bash2.body.is_empty(),
            "multi-line bash must carry the full command in the body"
        );
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
        assert!(
            small.body.is_empty(),
            "small unknown-tool arg must not duplicate in the body"
        );
        let big_cmd = "x".repeat(120);
        let big = tool_identity("mcp__x", &serde_json::json!({"q": big_cmd}), &p);
        assert!(
            !big.body.is_empty(),
            "long unknown-tool arg should carry full text in body"
        );
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
        assert_eq!(
            phi_tui::strip_ansi(&bare.anchor),
            "mcp__x",
            "{:?}",
            bare.anchor
        );
        assert!(
            !bare.body.is_empty(),
            "bare-string arg must show its value in the body"
        );
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

    // tool_max_lines = 0: one-line `✓ tool ╱ detail` summary, mirroring
    // pi-compact's compact tools. The full output preview is suppressed —
    // only the first non-empty line appears after `╱`.
    #[test]
    fn tools_tool_max_lines_zero_renders_one_line_summary() {
        let mut ui = test_ui(80, 24);
        ui.tools.tool_max_lines = 0;
        let rows = ui.tool_result_rows(
            0,
            "bash",
            &serde_json::json!({"cmd": "git status"}),
            "On branch main
nothing to commit
",
            usize::MAX,
            None,
            false,
        );
        let plain: String = rows
            .iter()
            .map(|l| phi_tui::strip_ansi(l))
            .collect::<Vec<_>>()
            .join(
                "
",
            );
        // Single-line summary with ✓ glyph and ╱ separator.
        assert!(plain.contains("✓"), "{plain}");
        assert!(plain.contains("╱"), "{plain}");
        assert!(plain.contains("git status"), "{plain}");
        // Only the first output line appears; the second is suppressed.
        assert!(plain.contains("On branch main"), "{plain}");
        assert!(!plain.contains("nothing to commit"), "{plain}");
    }

    // tool_padding = 0: no top/bottom padding lines around the tool card box.
    #[test]
    fn tools_tool_padding_zero_removes_box_padding() {
        let mut ui = test_ui(80, 24);
        ui.tools.tool_padding = 0;
        let rows = ui.tool_result_rows(
            0,
            "bash",
            &serde_json::json!({"cmd": "echo hi"}),
            "hi
",
            usize::MAX,
            None,
            false,
        );
        let plain: String = rows
            .iter()
            .map(|l| phi_tui::strip_ansi(l))
            .collect::<Vec<_>>()
            .join(
                "
",
            );
        // With padding = 0, the first and last lines should be content, not
        // blank bg-only lines. The anchor should be on line 0 (with its 1-space
        // left pad, but no blank top-padding line above it).
        let lines: Vec<&str> = plain.lines().collect();
        assert!(!lines.is_empty(), "should have content lines");
        assert!(
            lines[0].contains("echo"),
            "first line should be content: {plain}"
        );
    }

    // repl_max_lines = 0: REPL block shows only the "repl" header line with
    // no code body. Tool lines still appear underneath if tools are used.
    #[test]
    fn tools_repl_max_lines_zero_suppresses_code_body() {
        let mut ui = test_ui(80, 24);
        ui.tools.repl_max_lines = 0;
        ui.tools.tool_max_lines = 0;
        let rows = ui.repl_code_lines(0, "print(bash('ls -la'))", ui.code_limit());
        let plain: String = rows
            .iter()
            .map(|l| phi_tui::strip_ansi(l))
            .collect::<Vec<_>>()
            .join("\n");
        // No code lines at all when repl_max_lines = 0.
        assert!(
            rows.is_empty(),
            "repl_code_lines should return empty when max=0: {plain}"
        );
        assert!(
            !plain.contains("print"),
            "code should be suppressed: {plain}"
        );
    }

    // The repl block has no header, so neither a *live* action block (built
    // during an iteration) nor a persisted assistant message re-rendered as
    // prose after the run completes carries a `repl` or `turn` label — the
    // code and output stand on their own inside the boxed REPL block.
    #[test]
    fn repl_panel_label_carries_turn_only_for_live_blocks() {
        use tokio_util::sync::CancellationToken;
        let pending_plain = |ui: &mut Ui| -> String {
            pending_lines(ui)
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
        let live_plain = pending_plain(&mut ui);
        assert!(live_plain.contains("repl"), "{live_plain}");
        assert!(!live_plain.contains("turn"), "{live_plain}");
        assert!(live_plain.contains("print(1)"), "{live_plain}");

        // Persisted assistant message rendered after the run completes (run
        // no longer active): also plain, no `repl`/`turn` label. With native
        // tool calls, the assistant text is pure prose; code is replayed
        // separately via `push_repl_replay`.
        ui.running = None;
        ui.pending.clear();
        ui.push_assistant_message("Here is code that ran:");
        ui.push_repl_replay("print(1)", "1\n", "", 0.005, &[], &[]);
        let replay_plain = pending_plain(&mut ui);
        assert!(!replay_plain.contains("turn"), "{replay_plain}");
        assert!(replay_plain.contains("print(1)"), "{replay_plain}");
        assert!(replay_plain.contains("repl"), "{replay_plain}");
    }

    #[test]
    fn strip_tool_echoes_removes_printed_tool_result() {
        let tools = vec![session::ToolCall {
            name: "grep".to_owned(),
            args: serde_json::json!({"pattern": "exec"}),
            result: "crates/foo.rs:1: hello
"
            .to_owned(),
            full_output_path: None,
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
                full_output_path: None,
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
                full_output_path: None,
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
            full_output_path: None,
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
    fn collapse_answer_assignment_triple_quoted() {
        let code = "answer[\"content\"] = \"\"\"Hello world\nThis is a long answer.\"\"\"\nanswer[\"ready\"] = True";
        let result = collapse_answer_assignment(code);
        assert!(
            result.contains("<answer>"),
            "expected placeholder, got: {result}"
        );
        assert!(
            !result.contains("Hello world"),
            "answer text should be collapsed: {result}"
        );
        assert!(
            result.contains("answer[\"ready\"] = True"),
            "ready line should be preserved: {result}"
        );
    }

    #[test]
    fn collapse_answer_assignment_multi_line_triple_quoted() {
        let code = "answer[\"content\"] = \"\"\"\nLine 1\nLine 2\nLine 3\n\"\"\"\nanswer[\"ready\"] = True";
        let result = collapse_answer_assignment(code);
        assert!(
            result.contains("<answer>"),
            "expected placeholder: {result}"
        );
        assert!(
            !result.contains("Line 1"),
            "answer text should be collapsed: {result}"
        );
        assert!(
            !result.contains("Line 2"),
            "answer text should be collapsed: {result}"
        );
        assert!(
            result.contains("answer[\"ready\"] = True"),
            "ready line preserved: {result}"
        );
    }

    #[test]
    fn collapse_answer_assignment_single_quoted() {
        let code = "answer[\"content\"] = \"short answer\"";
        let result = collapse_answer_assignment(code);
        assert!(
            result.contains("<answer>"),
            "expected placeholder: {result}"
        );
        assert!(
            !result.contains("short answer"),
            "text should be collapsed: {result}"
        );
    }

    #[test]
    fn collapse_answer_assignment_preserves_other_code() {
        let code = "x = 1\nprint(x)\nanswer[\"content\"] = \"\"\"the answer\"\"\"\nanswer[\"ready\"] = True";
        let result = collapse_answer_assignment(code);
        assert!(result.contains("x = 1"), "other code preserved: {result}");
        assert!(
            result.contains("print(x)"),
            "other code preserved: {result}"
        );
        assert!(result.contains("<answer>"), "answer collapsed: {result}");
        assert!(!result.contains("the answer"), "answer text gone: {result}");
    }

    #[test]
    fn collapse_answer_assignment_no_answer_dict_unchanged() {
        let code = "x = 1\nprint(x)";
        let result = collapse_answer_assignment(code);
        assert_eq!(result, code);
    }

    #[test]
    fn collapse_answer_assignment_single_quotes_key() {
        let code = "answer['content'] = \"\"\"the answer\"\"\"\nanswer['ready'] = True";
        let result = collapse_answer_assignment(code);
        assert!(
            result.contains("<answer>"),
            "expected placeholder: {result}"
        );
        assert!(
            !result.contains("the answer"),
            "text should be collapsed: {result}"
        );
    }

    #[test]
    fn collapse_answer_assignment_repl_code_lines_collapses() {
        let ui = test_ui(80, 24);
        let code = "answer[\"content\"] = \"\"\"This is a very long answer that should not be shown in the code block.\"\"\"\nanswer[\"ready\"] = True";
        let rows = ui.repl_code_lines(0, code, usize::MAX);
        let plain: Vec<String> = rows.iter().map(|r| phi_tui::strip_ansi(r)).collect();
        let joined = plain.join("\n");
        assert!(
            joined.contains("<answer>"),
            "code block should show placeholder: {joined}"
        );
        assert!(
            !joined.contains("This is a very long answer"),
            "code block should NOT contain answer text: {joined}"
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
            .filter(|line| {
                line.starts_with('\u{250c}')
                    || line.starts_with('\u{2502}')
                    || line.starts_with('\u{2514}')
            })
            .collect();
        assert!(box_lines.is_empty(), "box lines should be gone: {plain:?}");

        // Line 0 is the separator, line 1 is the header, line 2 is the
        // chevron prompt with "hi".
        assert!(plain.len() >= 3, "{plain:?}");
        // Separator line is all box-drawing chars.
        assert!(
            plain[0].chars().all(|c| c == '\u{2500}'),
            "separator line: {plain:?}"
        );
        // Header contains the agent symbol text "phi"
        assert!(
            plain[1].contains("phi"),
            "header should contain agent: {plain:?}"
        );
        // Input line contains the editor text
        assert!(
            plain[2].contains("hi"),
            "input line should contain 'hi': {plain:?}"
        );

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
        //  - the agent label "phi"
        //  - the model id (stripped of provider prefix)
        let header = &plain[1];
        assert!(
            header.contains("phi"),
            "header should contain dir name: {header:?}"
        );
        assert!(
            header.contains("main"),
            "header should contain branch: {header:?}"
        );
        assert!(
            header.contains("phi"),
            "header should contain agent: {header:?}"
        );
        assert!(
            !header.contains("/tmp/phi"),
            "header should not contain full path: {header:?}"
        );

        // No footer line in non-framed mode.
        assert!(
            !plain.iter().any(|line| line.contains("thinking:")),
            "no footer in non-framed mode: {plain:?}"
        );
    }

    #[test]
    fn chrome_lines_ascii_fallback_when_nerd_fonts_disabled() {
        unsafe {
            std::env::set_var("PHI_NO_NERD_FONTS", "1");
        };
        let mut ui = test_ui(80, 24);
        ui.editor.set_text("hi");

        let (lines, _cursor) = ui.chrome_lines();
        let plain: Vec<String> = lines.iter().map(|line| phi_tui::strip_ansi(line)).collect();

        // In ASCII fallback mode the chevron is ">" and there are no Nerd
        // Font PUA codepoints in the output.
        assert!(plain[2].starts_with(">"), "ASCII chevron: {plain:?}");
        assert!(
            !plain.iter().any(|l| l.contains('\u{ea83}')),
            "no nerd folder icon: {plain:?}"
        );
        assert!(
            !plain.iter().any(|l| l.contains('\u{f013e}')),
            "no nerd chevron: {plain:?}"
        );
        unsafe {
            std::env::remove_var("PHI_NO_NERD_FONTS");
        };
    }

    #[test]
    fn chrome_lines_effort_label_width_thresholds() {
        assert_eq!(
            effort_label(ThinkingLevel::Medium, 80),
            Some("med".to_string())
        );
        assert_eq!(
            effort_label(ThinkingLevel::Medium, 100),
            Some("medium".to_string())
        );
        assert_eq!(effort_label(ThinkingLevel::Off, 80), None);
        assert_eq!(
            effort_label(ThinkingLevel::High, 50),
            Some("hig".to_string())
        );
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
        let lines = pending_lines(&mut ui);
        assert!(
            lines.iter().all(|line| !line.contains("Assistant")),
            "{:?}",
            lines
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

        // pi-style: assistant messages have no header label — the prose
        // is bare text separated by spacers.
        let lines = pending_lines(&mut ui);
        let assistant_cards = lines
            .iter()
            .filter(|line| line.contains("Assistant"))
            .count();
        assert_eq!(assistant_cards, 0, "{:?}", lines);
    }

    #[test]
    fn streamed_prose_around_block_is_not_recommitted_by_root_text() {
        // Regression: prose before an executed block was committed once by
        // `CodeExecStart` and a second time by `RootText`, leaving earlier
        // prose re-rendered after the code block. With tool calls, the
        // stream is pure prose (no fences); CodeExecStart flushes the
        // stream buffer as prose, then RootText flushes only the remaining
        // tail (which is empty when streaming completed).
        let mut ui = test_ui(80, 24);
        ui.running = Some(CancellationToken::new());
        ui.handle_rlm_event(RlmEvent::IterationStart {
            depth: 0,
            iteration: 0,
        });
        // The model streams: intro prose, then a code tool call fires.
        ui.handle_rlm_event(RlmEvent::RootDelta {
            depth: 0,
            iteration: 0,
            delta: "intro line".to_owned(),
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
            text: "intro line\n\ntrailing line".to_owned(),
        });

        let lines = pending_lines(&mut ui);
        let intro_cards = lines
            .iter()
            .filter(|line| line.contains("intro line"))
            .count();
        assert_eq!(intro_cards, 1, "intro prose duplicated: {:?}", lines);
        let trailing_cards = lines
            .iter()
            .filter(|line| line.contains("trailing line"))
            .count();
        assert_eq!(trailing_cards, 1, "{:?}", lines);
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
            result: Some("world".to_owned()),
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
        let plain = pending_lines(&mut ui)
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
            .append_entry(
                session::Role::Assistant,
                "done",
                assistant_display_text("done", "secret thought"),
                None,
                Vec::new(),
            )
            .unwrap();

        ui.replay_branch();
        let collapsed = pending_lines(&mut ui).join("\n");
        assert!(!collapsed.contains("secret thought"), "{collapsed}");

        ui.toggle_thinking();
        let expanded = pending_lines(&mut ui).join("\n");
        assert!(expanded.contains("secret thought"), "{expanded}");
        assert!(
            !expanded.contains("thinking expanded throughout transcript"),
            "{expanded}"
        );

        ui.toggle_thinking();
        let recollapsed = pending_lines(&mut ui).join("\n");
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
        let collapsed = pending_lines(&mut ui).join("\n");
        assert!(collapsed.contains("… (1 more lines)"), "{collapsed}");

        ui.toggle_tools();
        let expanded = pending_lines(&mut ui).join("\n");
        assert!(expanded.contains("line6"), "{expanded}");
        assert!(!expanded.contains("… (1 more lines)"), "{expanded}");
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

    // When a tool card is rendered inside a REPL block, its background
    // should be the slightly-brighter "nested" variant so the inner card
    // is distinguishable from the enclosing container (which uses the
    // base bg). A tool card rendered standalone (no block) uses the base.
    #[test]
    fn nested_tool_block_uses_lighter_background() {
        let ui = test_ui(80, 24);

        // Standalone tool card (no active block) → base success bg.
        let standalone = ui.tool_result_rows(
            0,
            "read_file",
            &serde_json::json!({"path": "x.rs"}),
            "ok",
            usize::MAX,
            Some("rust"),
            false,
        );
        assert!(
            standalone
                .iter()
                .any(|l| l.contains(&ui.palette.tool_success_bg)),
            "standalone tool card should use the base success bg"
        );
        assert!(
            !standalone
                .iter()
                .any(|l| l.contains(&ui.palette.tool_success_bg_nested)),
            "standalone tool card should NOT use the nested success bg"
        );

        // Nested tool card (block active) → nested (lighter) success bg.
        let mut ui = test_ui(80, 24);
        ui.begin_block();
        assert!(ui.block_active);
        let nested = ui.tool_result_rows(
            1,
            "read_file",
            &serde_json::json!({"path": "x.rs"}),
            "ok",
            usize::MAX,
            Some("rust"),
            false,
        );
        assert!(
            nested
                .iter()
                .any(|l| l.contains(&ui.palette.tool_success_bg_nested)),
            "nested tool card should use the lighter nested success bg"
        );
        assert!(
            !nested
                .iter()
                .any(|l| l.contains(&ui.palette.tool_success_bg)),
            "nested tool card should NOT use the base success bg"
        );
    }

    // Visual break rule: any two blocks must always have a one-line gap
    // between them — both inside a REPL block (between child tool cards) and
    // at the top level (between committed blocks).
    #[test]
    fn block_gap_between_child_tool_cards_inside_repl_block() {
        let mut ui = test_ui(80, 24);
        ui.begin_block();
        ui.push_repl_block(0, "print(1)", usize::MAX);
        // Push a tool card — should have a gap before it in the block.
        ui.push_tool_result(
            1,
            "read_file",
            &serde_json::json!({"path": "a.rs"}),
            "content a",
            usize::MAX,
            Some("rust"),
            false,
        );
        // Push a second tool card — should have a gap before it too.
        ui.push_tool_result(
            1,
            "read_file",
            &serde_json::json!({"path": "b.rs"}),
            "content b",
            usize::MAX,
            Some("rust"),
            false,
        );

        let plain: Vec<String> = ui.block.iter().map(|l| phi_tui::strip_ansi(l)).collect();

        // The code block (print(1)) is followed by a gap, then the first
        // tool card, then a gap, then the second tool card.
        let print_idx = plain
            .iter()
            .position(|l| l.contains("print(1)"))
            .expect("code in block");
        let card_a_idx = plain
            .iter()
            .position(|l| l.contains("a.rs"))
            .expect("first tool card");
        let card_b_idx = plain
            .iter()
            .rposition(|l| l.contains("b.rs"))
            .expect("second tool card");

        // There must be at least one blank line between the code and the
        // first card, and between the two cards.
        let gap_after_code = plain[print_idx + 1..card_a_idx]
            .iter()
            .any(|l| l.is_empty());
        assert!(
            gap_after_code,
            "no gap between code and first card: {plain:?}"
        );

        let gap_between_cards = plain[card_a_idx + 1..card_b_idx]
            .iter()
            .any(|l| l.is_empty());
        assert!(
            gap_between_cards,
            "no gap between two tool cards: {plain:?}"
        );
    }

    #[test]
    fn block_gap_between_top_level_blocks() {
        let mut ui = test_ui(80, 24);

        // User message → assistant prose → REPL block: all should be
        // separated by one-line gaps in pending.
        ui.push_user_message("hello");
        ui.push_prose("here is some code:");
        ui.push_repl_replay(
            "print(1)",
            "1
",
            "",
            0.005,
            &[],
            &[],
        );

        let plain: Vec<String> = pending_lines(&mut ui)
            .iter()
            .map(|l| phi_tui::strip_ansi(l))
            .collect();

        let user_idx = plain
            .iter()
            .position(|l| l.contains("hello"))
            .expect("user msg");
        let prose_idx = plain
            .iter()
            .position(|l| l.contains("here is some code"))
            .expect("prose");
        let repl_idx = plain
            .iter()
            .position(|l| l.contains("repl"))
            .expect("repl block");

        let gap_user_prose = plain[user_idx + 1..prose_idx].iter().any(|l| l.is_empty());
        assert!(gap_user_prose, "no gap between user and prose: {plain:?}");

        let gap_prose_repl = plain[prose_idx + 1..repl_idx].iter().any(|l| l.is_empty());
        assert!(gap_prose_repl, "no gap between prose and repl: {plain:?}");
    }

    #[test]
    fn ensure_pending_gap_does_not_double_up() {
        let mut ui = test_ui(80, 24);
        ui.pending
            .push(TranscriptEntry::RawLines(vec!["first line".to_owned()]));
        ui.ensure_pending_gap();
        assert_eq!(ui.pending.len(), 2);
        assert!(matches!(
            &ui.pending[1],
            TranscriptEntry::RawLines(lines) if lines.first().is_some_and(|l| l.is_empty())
        ));
        // Second call should NOT add another blank line.
        ui.ensure_pending_gap();
        assert_eq!(ui.pending.len(), 2);
    }

    #[test]
    fn ensure_block_gap_does_not_double_up() {
        let mut ui = test_ui(80, 24);
        ui.block_active = true;
        ui.block.push("first row".to_owned());
        ui.ensure_block_gap();
        assert_eq!(ui.block.len(), 2);
        assert!(ui.block[1].is_empty());
        // Second call should NOT add another blank line.
        ui.ensure_block_gap();
        assert_eq!(ui.block.len(), 2);
    }

    #[test]
    fn resize_renders_transcript_entries_at_new_width() {
        let mut ui = test_ui(80, 24);
        // Store a user message and an assistant message with a long paragraph
        // that will wrap differently at width 80 vs width 120.
        let long_para = "the quick brown fox jumps over the lazy dog and then keeps running through the forest because it is very energetic and wants to find the other foxes who are waiting near the old oak tree by the riverside where they have been gathering all afternoon to discuss important matters of state and plan their next big adventure together with great enthusiasm.";
        ui.store.append(session::Role::User, "hello").unwrap();
        ui.store
            .append_entry(
                session::Role::Assistant,
                long_para,
                Some(long_para.to_owned()),
                None,
                Vec::new(),
            )
            .unwrap();

        // Populate transcript_entries from the store.
        ui.replay_branch();
        // Move pending to transcript_entries (as draw() would).
        ui.flush_pending_to_transcript();
        let entries = ui.transcript_entries.clone();

        // Render entries at width 80 (narrow).
        ui.term.resize(80, 24);
        let narrow: Vec<String> = entries
            .iter()
            .flat_map(|e| {
                let rendered = ui.render_entry(e);
                rendered
                    .into_iter()
                    .map(|l| phi_tui::strip_ansi(&l))
                    .collect::<Vec<_>>()
            })
            .collect();
        let narrow_nonblank: Vec<&String> = narrow.iter().filter(|l| !l.is_empty()).collect();
        let narrow_max = narrow_nonblank.iter().map(|l| l.len()).max().unwrap_or(0);
        let narrow_lines = narrow_nonblank.len();

        // Render entries at width 120 (wide) — same entries, new width.
        ui.term.resize(120, 24);
        let wide: Vec<String> = entries
            .iter()
            .flat_map(|e| {
                let rendered = ui.render_entry(e);
                rendered
                    .into_iter()
                    .map(|l| phi_tui::strip_ansi(&l))
                    .collect::<Vec<_>>()
            })
            .collect();
        let wide_nonblank: Vec<&String> = wide.iter().filter(|l| !l.is_empty()).collect();
        let wide_max = wide_nonblank.iter().map(|l| l.len()).max().unwrap_or(0);
        let wide_lines = wide_nonblank.len();

        // The re-rendered content should use more columns (wider max line)
        // and fewer lines than the narrow version — proving the transcript
        // flexes to the terminal width on every render, like Pi.
        assert!(
            wide_max > narrow_max,
            "wide max width {wide_max} should exceed narrow max width {narrow_max}"
        );
        assert!(
            wide_lines < narrow_lines,
            "wide line count {wide_lines} should be less than narrow line count {narrow_lines}"
        );
    }
}
