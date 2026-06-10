//! Interactive chat: an inline TUI (phi-style scrollback + live region)
//! driving a persistent `RlmSession` as a coding agent. Auth is handled
//! in-TUI via /login, mirroring ref/phi: the OAuth flow runs in the
//! background, the URL lands in scrollback, and the editor doubles as the
//! paste-the-redirect-URL input (racing the localhost callback server).

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use phi_ai::{
    AuthStorage, LoginCallbacks, LoginMethod, Model, OAuthProviderId, Registry, ThinkingLevel,
    Usage,
};
use phi_rlm::{
    ContextPayload, ProviderLm, ReplResult, Rlm, RlmCompletion, RlmEvent, StopCause,
    split_at_first_block, split_prose,
};
use phi_tui::crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use phi_tui::{
    Editor, EditorAction, LiveCursor, RawModeGuard, Selector, SelectorItem, SelectorOutcome,
    Spinner, Terminal, fuzzy_filter, render_markdown, wrap_ansi,
};

use crate::files::{self, FileSearcher};
use crate::session::{self, SessionStore};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const CYAN: &str = "\x1b[36m";
const YELLOW: &str = "\x1b[33m";

const CODE_PREVIEW_LINES: usize = 8;
const OUTPUT_PREVIEW_LINES: usize = 5;

/// Byte length of the prefix of `text` that is safe to flush into scrollback
/// while streaming: it ends at a completed blank line, leaves no code fence
/// open, and stops before the first complete ```repl block (which
/// `CodeExecStart` will consume from the stream).
fn stable_prefix(text: &str) -> usize {
    let limit = split_at_first_block(text).map_or(text.len(), |(prose, _)| prose.len());
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

/// Gutter for the first line of an action at `depth`: one dim `│` column per
/// enclosing level, then a cyan `⏺` anchor.
fn dot_prefix(depth: usize) -> String {
    format!("{DIM}{}{RESET}{CYAN}⏺{RESET} ", "│ ".repeat(depth))
}

/// Gutter for continuation/result lines at `depth`: depth+1 dim `│` columns.
fn bar_prefix(depth: usize) -> String {
    format!("{DIM}{}{RESET}", "│ ".repeat(depth + 1))
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
        exchanges: Vec<(String, String)>,
    },
    Shutdown,
}

enum Done {
    Run(RlmCompletion),
    Shell {
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

pub struct ChatSetup {
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

pub async fn run(setup: ChatSetup) -> Result<()> {
    let ChatSetup {
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
                    if done_tx.send(Done::Shell { record, result }).is_err() {
                        break;
                    }
                }
                Cmd::Reset => session.reset().await,
                Cmd::Restore { exchanges } => {
                    let result = session.restore_history(&exchanges).await;
                    if done_tx.send(Done::Restored(result)).is_err() {
                        break;
                    }
                }
                Cmd::Shutdown => break,
            }
        }
        session.shutdown().await;
    });

    let guard = RawModeGuard::new()?;
    let (width, height) = phi_tui::crossterm::terminal::size().unwrap_or((80, 24));
    let mut ui = Ui::new(
        Terminal::new(std::io::stdout(), width, height),
        ui_setup,
        login_tx,
    );
    let mut input = EventStream::new();
    let mut ticker = tokio::time::interval(Duration::from_millis(100));
    // Coalesce redraws to ~60fps (pi-style): bursts of stream deltas collapse
    // into one frame instead of repainting per SSE chunk.
    const FRAME: Duration = Duration::from_millis(16);
    let mut last_draw = tokio::time::Instant::now();
    let mut dirty = false;

    // Terminal title (OSC 0), pi-style.
    {
        use std::io::Write as _;
        let mut out = std::io::stdout();
        let _ = write!(out, "\x1b]0;phi · {}\x07", display_path(&ui.cwd));
        let _ = out.flush();
    }

    ui.greet(&missing_credentials);
    ui.draw()?;

    loop {
        use futures::StreamExt;
        let quit = tokio::select! {
            maybe = input.next() => match maybe {
                #[cfg(unix)]
                Some(Ok(Event::Key(key)))
                    if key.kind != KeyEventKind::Release
                        && key.code == KeyCode::Char('z')
                        && key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    let _ = ui.term.finish();
                    guard.suspend();
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
            drop(input);
            let text = ui.editor.expanded_text();
            let result = guard.with_suspended(|| external_editor(&text));
            input = EventStream::new();
            match result {
                Ok(Some(new_text)) => {
                    ui.editor.set_text(new_text.trim_end_matches('\n'));
                    ui.refresh_popup();
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
    println!("bye");
    Ok(())
}

/// Write `text` to a temp file, open it in $VISUAL/$EDITOR, and return the
/// edited content (None when the editor exits non-zero).
fn external_editor(text: &str) -> Result<Option<String>, String> {
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
    term: Terminal<std::io::Stdout>,
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
    /// Follow-up prompts queued while a turn is in flight; sent FIFO as
    /// turns complete.
    queued: Vec<String>,
    /// Time of the last ctrl-c; a second one within 500ms quits.
    last_ctrl_c: Option<std::time::Instant>,
    /// Lines to flush into scrollback on the next draw.
    pending: Vec<String>,
    /// Whether the last scrollback line was gutter activity; a blank line is
    /// inserted before a depth-0 action that follows prose.
    last_gutter: bool,
    /// Streamed root-model text for the current iteration, minus the
    /// paragraphs/fences already flushed to scrollback.
    stream: String,
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
        term: Terminal<std::io::Stdout>,
        setup: UiSetup,
        login_tx: mpsc::UnboundedSender<LoginMsg>,
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
        let cwd = std::env::current_dir().unwrap_or_default();
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
            queued: Vec::new(),
            last_ctrl_c: None,
            pending: Vec::new(),
            last_gutter: false,
            stream: String::new(),
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
        if self.running.is_some() {
            let mut tail = render_markdown(self.stream.trim(), self.markdown_width());
            let editor_visible = self
                .editor
                .render()
                .0
                .len()
                .min((self.term.height() * 3 / 10).max(5));
            let reserved = 5 + editor_visible + self.queued.len();
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
        let rule = |indicator: String| {
            if indicator.is_empty() {
                format!("{DIM}{}{RESET}", "─".repeat(width))
            } else {
                let pad = width.saturating_sub(indicator.len());
                format!(
                    "{DIM}───{indicator}{}{RESET}",
                    "─".repeat(pad.saturating_sub(3))
                )
            }
        };

        let cursor;
        if let Some((_, widget)) = &self.selector {
            // Selector replaces the editor area (pi's showSelector).
            let (sel_lines, sel_cursor) = widget.render(width);
            let top = lines.len() + 1;
            lines.push(rule(String::new()));
            cursor = sel_cursor.map(|(line, col)| LiveCursor {
                line: top + line,
                col,
            });
            lines.extend(sel_lines);
            lines.push(rule(String::new()));
            let hint = if widget.searchable() {
                "type to filter · up/down select · enter confirm · esc cancel"
            } else {
                "up/down select · enter confirm · esc cancel"
            };
            lines.push(format!("{DIM}{hint}{RESET}"));
        } else {
            // Editor viewport, pi-style: at most 30% of the terminal (min 5
            // lines), scrolled to keep the cursor visible, with `↑/↓ N more`
            // indicators in the rules.
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
            let below = editor_lines.len() - end;

            let editor_top = lines.len() + 1;
            let above = self.editor_scroll;
            lines.push(rule(if above > 0 {
                format!(" ↑ {above} more ")
            } else {
                String::new()
            }));
            for (i, line) in editor_lines[self.editor_scroll..end].iter().enumerate() {
                if i + self.editor_scroll == 0 {
                    lines.push(format!("{BOLD}›{RESET} {line}"));
                } else {
                    lines.push(format!("  {line}"));
                }
            }
            lines.push(rule(if below > 0 {
                format!(" ↓ {below} more ")
            } else {
                String::new()
            }));
            cursor = Some(LiveCursor {
                line: editor_top + (row - self.editor_scroll),
                col: col + 2,
            });

            for text in &self.queued {
                let first = text.lines().next().unwrap_or("");
                let more = if text.lines().count() > 1 { " …" } else { "" };
                lines.push(format!("{DIM}queued › {first}{more}{RESET}"));
            }

            if let Some(popup) = &self.popup {
                lines.extend(render_popup(popup));
            }

            // Contextual hint, shown only when there is a mode to explain.
            let hint = if self.login.is_some() {
                Some(
                    "finish login in the browser, or paste the redirect URL and press enter · esc cancels",
                )
            } else if self.popup.is_some() {
                Some("tab complete · up/down select · enter accept · esc dismiss")
            } else if self.running.is_some() {
                Some("enter queue follow-up · alt+up edit queued · esc cancel")
            } else {
                None
            };
            if let Some(hint) = hint {
                lines.push(format!("{DIM}{hint}{RESET}"));
            }
        }

        // Persistent footer (pi-style): pwd (branch), then stats left and
        // model · thinking right.
        if self.branch_at.elapsed() > Duration::from_secs(5) {
            self.branch = git_branch(&self.cwd);
            self.branch_at = std::time::Instant::now();
        }
        let mut pwd = display_path(&self.cwd);
        if let Some(branch) = &self.branch {
            pwd.push_str(&format!(" ({branch})"));
        }
        lines.push(format!("{DIM}{pwd}{RESET}"));
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
        let right = format!(
            "{} · thinking:{}",
            self.model_label,
            self.root_lm.thinking().as_str()
        );
        let used = phi_tui::visible_width(&left) + phi_tui::visible_width(&right);
        if used < width {
            lines.push(format!(
                "{DIM}{left}{}{right}{RESET}",
                " ".repeat(width - used)
            ));
        } else {
            lines.push(format!("{DIM}{left} {right}{RESET}"));
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
        let exchanges = self.store.branch_exchanges();
        let _ = cmd_tx.send(Cmd::Restore { exchanges });
    }

    /// Re-render the active branch into scrollback, mirroring how prompts
    /// and responses looked live.
    fn replay_branch(&mut self) {
        let entries: Vec<(session::Role, String)> = self
            .store
            .branch()
            .into_iter()
            .map(|e| (e.role, e.text.clone()))
            .collect();
        for (role, text) in entries {
            match role {
                session::Role::User => {
                    self.pending.push(String::new());
                    for (i, line) in text.lines().enumerate() {
                        let prefix = if i == 0 { "›" } else { " " };
                        self.pending.push(format!("{BOLD}{prefix} {line}{RESET}"));
                    }
                    self.last_gutter = false;
                }
                session::Role::Assistant => self.push_prose(&text),
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
                    self.open_model_selector(true);
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
                    "{DIM}      ctrl-y yank · ctrl-_ undo · ctrl-p cycle model · ctrl-l models · shift+tab thinking{RESET}"
                ));
                self.pending.push(format!(
                    "{DIM}      ctrl-o expand tool output · ctrl-g $EDITOR · ctrl-z suspend · esc cancel · ctrl-c ctrl-c quit{RESET}"
                ));
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
            _ if trimmed == "/model" || trimmed.starts_with("/model ") => {
                let query = trimmed.strip_prefix("/model").unwrap().trim();
                if query.is_empty() {
                    self.open_model_selector(true);
                } else {
                    self.switch_model(query, true);
                }
            }
            _ if trimmed == "/submodel" || trimmed.starts_with("/submodel ") => {
                let query = trimmed.strip_prefix("/submodel").unwrap().trim();
                if query.is_empty() {
                    self.open_model_selector(false);
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
            self.push_dot(0, "repl".to_owned());
            self.push_preview(0, &code, self.code_limit());
        } else {
            self.pending.push(String::new());
            self.pending.push(format!("{BOLD}${RESET} {cmd}"));
            self.last_gutter = false;
        }
        let _ = cmd_tx.send(Cmd::Shell { code, record });
    }

    fn send_prompt(&mut self, prompt: &str, cmd_tx: &mpsc::UnboundedSender<Cmd>) {
        self.pending.push(String::new());
        for (i, line) in prompt.lines().enumerate() {
            let prefix = if i == 0 { "›" } else { " " };
            self.pending.push(format!("{BOLD}{prefix} {line}{RESET}"));
        }
        self.last_gutter = false;
        if let Err(e) = self.store.append(session::Role::User, prompt) {
            self.pending
                .push(format!("{YELLOW}session not saved:{RESET} {e}"));
        }
        let cancel = CancellationToken::new();
        self.running = Some(cancel.clone());
        self.turn = 0;
        self.stream.clear();
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

    /// First line of an action block: gutter columns, then a `⏺` anchor.
    /// Wrapped continuations keep the gutter. A depth-0 action that follows
    /// prose gets a separating blank line first.
    fn push_dot(&mut self, depth: usize, text: String) {
        if depth == 0 && !self.last_gutter {
            self.pending.push(String::new());
        }
        let mut rows = wrap_ansi(&text, self.gutter_text_width(depth)).into_iter();
        let first = rows.next().unwrap_or_default();
        self.pending
            .push(format!("{}{DIM}{first}{RESET}", dot_prefix(depth)));
        for row in rows {
            self.pending
                .push(format!("{}{DIM}{row}{RESET}", bar_prefix(depth)));
        }
        self.last_gutter = true;
    }

    /// Continuation/result line inside an action block; wraps to the
    /// terminal width so every row keeps the gutter.
    fn push_bar(&mut self, depth: usize, text: String) {
        for row in wrap_ansi(&text, self.gutter_text_width(depth)) {
            self.pending
                .push(format!("{}{DIM}{row}{RESET}", bar_prefix(depth)));
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

    /// Render finalized prose (markdown) into scrollback.
    fn push_prose(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        self.pending.push(String::new());
        self.pending
            .extend(render_markdown(text, self.markdown_width()));
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
            RlmEvent::RootText { depth: 0, text, .. } => {
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
                }
                self.push_dot(depth, "repl".to_owned());
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
                    self.status = "thinking...".to_owned();
                }
            }
            RlmEvent::SubcallStart {
                depth,
                model,
                prompt_preview,
            } => {
                self.push_dot(depth, format!("{model} ← \"{prompt_preview}\""));
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
                let text = match error {
                    Some(error) => format!("{model} failed: {error}"),
                    None => format!("{model} done {:.1}s", duration.as_secs_f64()),
                };
                self.push_bar(depth, text);
            }
            RlmEvent::RunStart { depth } if depth > 0 => {
                self.push_dot(depth, "rlm".to_owned());
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
            Done::Shell { record, result } => self.handle_shell_done(record, result),
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

    fn handle_shell_done(&mut self, record: bool, result: Result<ReplResult, String>) {
        match result {
            Ok(res) if record => {
                self.push_bar(0, format!("exec {:.1}s", res.duration.as_secs_f64()));
                self.push_preview(0, &res.stdout, self.output_limit());
                self.push_preview(0, &res.stderr, self.output_limit());
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
                self.pending.push(format!("\x1b[31merror:{RESET} {e}"));
                self.last_gutter = false;
            }
        }
    }

    fn handle_run_done(&mut self, completion: RlmCompletion) {
        self.running = None;
        self.stream.clear();
        self.pending.push(String::new());
        if completion.stop == StopCause::Failed {
            let error = completion.error.unwrap_or_else(|| "run failed".to_owned());
            self.pending.push(format!("\x1b[31merror:{RESET} {error}"));
        } else if !completion.response.is_empty() {
            self.pending
                .extend(render_markdown(&completion.response, self.markdown_width()));
        }
        // Mirror the env's history condition so the store and the REPL's
        // history_N variables stay aligned.
        if completion.stop != StopCause::Failed && !completion.response.trim().is_empty() {
            let _ = self
                .store
                .append(session::Role::Assistant, &completion.response);
        }
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
