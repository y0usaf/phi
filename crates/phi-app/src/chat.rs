//! Inline interactive chat frontend (PLAN.md WS3.1) — the first frontend
//! pack. It bridges the agent's event stream to phi-tui's scrollback +
//! live-region terminal; the substrate never renders (DESIGN.md frontend
//! seam). forgecode-minimal chrome: `❯` editor at the bottom (no borders),
//! streamed assistant markdown flushed to scrollback per block, one `•`
//! line per tool call (output hidden unless it failed), braille spinner +
//! dim elapsed while a turn runs, one dim status line (model · tokens ·
//! cost). Esc interrupts the run; Enter mid-run steers at the next turn
//! boundary; Ctrl+C clears the editor (quits when empty); Ctrl+D quits.

use std::io::Stdout;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use phi_agent::{AgentEvent, RunOutcome, SteeringQueue, run_flat};
use phi_ai::{
    AssistantEvent, Auth, AuthStorage, Context, LoginCallbacks, LoginMethod, Message, Model,
    OAuthProviderId, Registry, StopReason, StreamOptions, ThinkingLevel, Usage, UserMessage,
};
use phi_host::Host;
use phi_tui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind};
use phi_tui::style::{BOLD, CYAN, DIM, RESET};
use phi_tui::{
    Editor, EditorAction, FOCUS_MARKER as FOCUS, LiveCursor, MarkdownPalette, PROMPT, RawModeGuard,
    SPINNER_FRAME_MS, Terminal, fuzzy_match, render_markdown, spinner_frame_at, visible_width,
};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::format::{format_tokens, format_usage_summary};
use crate::transcript;
use crate::tree_overlay;

/// Double-Esc window (pi's `lastEscapeTime < 500`).
const DOUBLE_ESC_MS: u128 = 500;

/// A modal overlay that preempts the editor + run keys while open. Pi's
/// tree/fork selectors swap in over the editor; phi renders them as the
/// live region and routes keys to the widget, returning control on Esc or
/// confirm.
enum Overlay {
    /// The session-tree browser (`/tree`, double-Esc). Navigates the leaf
    /// in place on Enter. Boxed: the browser is the biggest variant by
    /// far, so a flat enum would pad every `Option<Overlay>`.
    Tree(Box<tree_overlay::Browser>),
    /// The fork selector (`/fork`): a fuzzy-searchable list of user
    /// messages; Enter forks to a new session file and seeds the editor.
    Fork(phi_tui::Selector),
}

/// One row in the fork selector (value = entry id, label = message text).
struct ForkItem {
    value: String,
    label: String,
    detail: String,
}

/// A cheap total-entry count for the fork selector's `#i of N` detail where
/// N is path length (fine — pi shows user-message ordinal, which for a
/// linear path is entry ordinal). Kept simple; accuracy isn't critical.
fn path_len_hint(tree: &[phi_session::TreeNode]) -> usize {
    tree.iter().map(count_nodes).sum()
}

fn count_nodes(n: &phi_session::TreeNode) -> usize {
    1 + n.children.iter().map(count_nodes).sum::<usize>()
}

pub struct ChatSpec {
    pub host: Arc<Host>,
    pub registry: Arc<Registry>,
    pub model: Model,
    pub auth: Auth,
    /// Credential store, for `/login`, `/logout`, and the auth re-resolve a
    /// `/model` switch needs.
    pub storage: Arc<AuthStorage>,
    pub thinking: ThinkingLevel,
    /// Replaces the prompt head; project context + date/cwd tail stays.
    pub system: Option<String>,
    /// Show tool output even when it succeeded (PLAN.md WS3.2).
    pub verbose: bool,
    /// Session-level cancellation (external SIGINT): quits the frontend and
    /// winds down any active run (run tokens are children of this one).
    pub cancel: CancellationToken,
    /// Sessions root (phi data dir). Every chat session writes one JSONL
    /// log; `--continue` resumes the most recent for cwd (PLAN.md WS4.1).
    pub sessions_root: std::path::PathBuf,
    /// Resume the most recent session for cwd on startup.
    pub continue_session: bool,
}

pub async fn run(spec: ChatSpec) -> Result<()> {
    // Panics unwind through raw mode; restore the terminal before the
    // default hook prints, or the report lands on a broken screen.
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        phi_tui::emergency_restore();
        prev_hook(info);
    }));

    let guard = RawModeGuard::new().context("entering raw mode")?;
    // Some ptys report Ok((0, 0)); treat that like a failed query.
    let (width, height) = match phi_tui::crossterm::terminal::size() {
        Ok((w, h)) if w > 0 && h > 0 => (w, h),
        _ => (80, 24),
    };
    let mut term = Terminal::new(std::io::stdout(), width, height);

    let (tx, mut rx) = mpsc::unbounded_channel::<UiMsg>();
    spawn_input_thread(tx.clone());

    // Session log (PLAN.md WS4.1): seed the transcript from the prior
    // session on `--continue`, then create/open the store. Done before
    // `Chat::new` so the store and transcript live with the `Chat`.
    let cwd = std::env::current_dir().context("resolving cwd")?;
    let model_key = format!("{}/{}", spec.model.provider, spec.model.id);
    let mut ctx = Box::<Context>::default();
    let store = crate::session::open(
        &spec.sessions_root,
        &cwd,
        &model_key,
        &mut ctx,
        spec.continue_session,
    )?;

    let mut chat = Chat::new(spec, tx, width as usize, height as usize, ctx, store);
    let mut tick = tokio::time::interval(Duration::from_millis(SPINNER_FRAME_MS));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        let overlay = chat.overlay_lines();
        let (live, maybe_cursor) = chat
            .state
            .live_lines_with_overlay(chat.state.elapsed_ms(), overlay.as_deref());
        let cursor = maybe_cursor.unwrap_or(LiveCursor { line: 0, col: 0 });
        term.render(&chat.state.take_pending(), &live, Some(cursor))?;
        let flow = tokio::select! {
            _ = chat.spec.cancel.cancelled() => Flow::Quit,
            _ = tick.tick() => Flow::Continue,
            msg = rx.recv() => match msg {
                None => Flow::Quit,
                Some(msg) => chat.handle(msg, &mut term)?,
            },
        };
        if matches!(flow, Flow::Quit) {
            break;
        }
    }

    // Winds down an in-flight run (stream + tools) via the token tree.
    chat.spec.cancel.cancel();
    term.render(&chat.state.take_pending(), &[], None)?;
    term.finish()?;
    drop(guard);
    phi_tui::drain_input();
    println!("{}", format_usage_summary(&chat.state.usage));
    Ok(())
}

enum Flow {
    Continue,
    Quit,
}

enum UiMsg {
    Input(Event),
    Agent(AgentEvent),
    /// The run task returns the context it owned for the run's duration.
    RunDone(Box<Context>, Result<RunOutcome>),
    /// A slash command's result, back from the host's VM thread.
    Command {
        name: String,
        result: Result<Option<Value>>,
    },
    /// A dim `•` note from an async command task (login progress etc.).
    Notice(String),
    /// An async command task failed; rendered as an error line.
    CommandFailed(String),
    /// `/model` resolved: model and its auth swap together.
    ModelChanged {
        model: Model,
        auth: Auth,
    },
    /// Fresh credentials for the current model after a `/login`.
    AuthRefreshed(Auth),
    LoginDone {
        provider: OAuthProviderId,
        result: Result<(), String>,
    },
}

/// Blocking stdin reader on its own thread; raw-mode key events don't mix
/// with tokio, and a dropped receiver ends the loop. The thread dies with
/// the process when it is parked in `read` at exit.
fn spawn_input_thread(tx: mpsc::UnboundedSender<UiMsg>) {
    std::thread::spawn(move || {
        while let Ok(event) = phi_tui::crossterm::event::read() {
            if tx.send(UiMsg::Input(event)).is_err() {
                break;
            }
        }
    });
}

struct Chat {
    spec: ChatSpec,
    tx: mpsc::UnboundedSender<UiMsg>,
    state: ChatState,
    steering: SteeringQueue,
    /// Home for the transcript while no run is active; a run task owns it
    /// for the run's duration and returns it via `RunDone`.
    ctx: Option<Box<Context>>,
    run_cancel: Option<CancellationToken>,
    /// An in-flight `/login` flow; Esc cancels it like a run.
    login_cancel: Option<CancellationToken>,
    /// The session log (PLAN.md WS4.1). Shared so each spawned run task can
    /// flush its message delta off the UI loop.
    store: crate::session::SharedStore,
    /// A modal overlay pre-preempting the editor (`/tree`, `/fork`).
    overlay: Option<Overlay>,
    /// Wall-clock of the last Esc with an empty editor (double-Esc = /tree).
    last_escape: Option<Instant>,
    /// Terminal height, tracked from construction + resize so the tree
    /// overlay sizes its visible rows without a `Terminal` borrow.
    height: usize,
}

impl Chat {
    fn new(
        spec: ChatSpec,
        tx: mpsc::UnboundedSender<UiMsg>,
        width: usize,
        height: usize,
        ctx: Box<Context>,
        store: crate::session::SharedStore,
    ) -> Self {
        let mut state = ChatState::new(spec.model.id.clone(), width, &spec.host);
        state.verbose = spec.verbose;
        Self {
            spec,
            tx,
            state,
            steering: Arc::default(),
            ctx: Some(ctx),
            run_cancel: None,
            login_cancel: None,
            store,
            overlay: None,
            last_escape: None,
            height,
        }
    }

    fn handle(&mut self, msg: UiMsg, term: &mut Terminal<Stdout>) -> Result<Flow> {
        match msg {
            UiMsg::Input(event) => return self.handle_input(event, term),
            UiMsg::Agent(event) => self.state.on_agent_event(event),
            UiMsg::RunDone(ctx, outcome) => self.on_run_done(ctx, &outcome),
            UiMsg::Command { name, result } => return self.on_command_done(&name, result, term),
            UiMsg::Notice(text) => self.state.push_notice(&text),
            UiMsg::CommandFailed(text) => {
                self.state.push_line(transcript::error_line(&text, true));
            }
            UiMsg::ModelChanged { model, auth } => {
                self.state.model_name = model.id.clone();
                self.state
                    .push_notice(&format!("model set to {}/{}", model.provider, model.id));
                self.spec.model = model;
                self.spec.auth = auth;
            }
            UiMsg::AuthRefreshed(auth) => self.spec.auth = auth,
            UiMsg::LoginDone { provider, result } => self.on_login_done(provider, &result),
        }
        Ok(Flow::Continue)
    }

    fn handle_input(&mut self, event: Event, term: &mut Terminal<Stdout>) -> Result<Flow> {
        match event {
            Event::Key(key) => {
                if key.kind == KeyEventKind::Release {
                    return Ok(Flow::Continue);
                }
                // A modal overlay (`/tree`, `/fork`) owns all keys while
                // open: arrows navigate, Esc closes, Enter confirms.
                if let Some(outcome) = self.handle_overlay_key(key, term)? {
                    return Ok(outcome);
                }
                // When the slash-completion popup is open it gets first
                // claim on Tab/Up/Down/Esc — preempting the editor's
                // history navigation and the run-interrupt on Esc.
                // Enter falls through after completion: it completes the
                // prefix, then the editor's own Enter produces Submit.
                if self.state.slash_completions.is_open() {
                    let ctrl = key
                        .modifiers
                        .contains(phi_tui::crossterm::event::KeyModifiers::CONTROL);
                    match key.code {
                        KeyCode::Tab if !ctrl => {
                            self.state
                                .slash_completions
                                .complete_selected(&mut self.state.editor);
                            self.state
                                .slash_completions
                                .sync_from_editor(self.state.editor.before_cursor());
                            return Ok(Flow::Continue);
                        }
                        KeyCode::Down if !ctrl => {
                            self.state.slash_completions.move_selection(1);
                            return Ok(Flow::Continue);
                        }
                        KeyCode::Up | KeyCode::BackTab if !ctrl => {
                            self.state.slash_completions.move_selection(-1);
                            return Ok(Flow::Continue);
                        }
                        KeyCode::Esc => {
                            self.state
                                .slash_completions
                                .dismiss(self.state.editor.before_cursor());
                            return Ok(Flow::Continue);
                        }
                        KeyCode::Enter if !ctrl => {
                            self.state
                                .slash_completions
                                .complete_selected(&mut self.state.editor);
                            // Fall through: the editor produces Submit with
                            // the completed text.
                        }
                        _ => {}
                    }
                }
                if key.code == KeyCode::Esc {
                    if let Some(cancel) = &self.run_cancel {
                        cancel.cancel();
                    }
                    if let Some(cancel) = &self.login_cancel {
                        cancel.cancel();
                    }
                    // Double-Esc with an empty editor opens the tree (pi's
                    // default `doubleEscapeAction: "tree"`). A run/login
                    // intercept already cancelled above lands its interrupt;
                    // the double-tap path only fires when idle + empty.
                    if self.run_cancel.is_none()
                        && self.login_cancel.is_none()
                        && self.state.editor.is_empty()
                    {
                        let now = Instant::now();
                        if self
                            .last_escape
                            .is_some_and(|t| now.duration_since(t).as_millis() < DOUBLE_ESC_MS)
                        {
                            self.last_escape = None;
                            self.open_tree();
                            return Ok(Flow::Continue);
                        }
                        self.last_escape = Some(now);
                    }
                    return Ok(Flow::Continue);
                }
                // Ctrl-T: toggle reasoning-trace visibility (pi's
                // `app.thinking.toggle`). Pre-empts the editor so 't'
                // isn't inserted; the model still thinks, the trace is
                // just unrendered.
                if key.code == KeyCode::Char('t')
                    && key
                        .modifiers
                        .contains(phi_tui::crossterm::event::KeyModifiers::CONTROL)
                {
                    self.state.show_thinking = !self.state.show_thinking;
                    self.state.push_notice(if self.state.show_thinking {
                        "thinking: shown"
                    } else {
                        "thinking: hidden"
                    });
                    return Ok(Flow::Continue);
                }
                // Shift-Tab (BackTab): cycle thinking level (pi's
                // `app.thinking.cycle`). Only meaningful for reasoning
                // models; the slash-completion popup claims BackTab for
                // up-navigation when open, so this fires only when closed.
                if key.code == KeyCode::BackTab {
                    let levels = self.spec.model.supported_thinking_levels();
                    if levels.len() <= 1 {
                        self.state
                            .push_notice("current model does not support thinking");
                    } else {
                        let cur = self.spec.thinking;
                        let idx = levels.iter().position(|&l| l == cur).unwrap_or(0);
                        let next = levels[(idx + 1) % levels.len()];
                        self.spec.thinking = next;
                        self.state
                            .push_notice(&format!("thinking: {}", next.as_str()));
                    }
                    return Ok(Flow::Continue);
                }
                match self.state.editor.handle_key(key) {
                    EditorAction::Submit(text) => {
                        self.state.slash_completions.visible = false;
                        self.submit(text);
                    }
                    EditorAction::Cancel => {
                        // Ctrl+C: clear a non-empty editor, quit on an
                        // empty one (any active run winds down on exit).
                        if self.state.editor.is_empty() {
                            return Ok(Flow::Quit);
                        }
                        self.state.editor.clear();
                        self.state
                            .slash_completions
                            .sync_from_editor(self.state.editor.before_cursor());
                    }
                    EditorAction::Quit => return Ok(Flow::Quit),
                    EditorAction::None => {
                        self.state
                            .slash_completions
                            .sync_from_editor(self.state.editor.before_cursor());
                    }
                }
            }
            Event::Paste(text) => {
                self.state.editor.insert_paste(&text);
                self.state
                    .slash_completions
                    .sync_from_editor(self.state.editor.before_cursor());
            }
            Event::Resize(w, h) => {
                term.resize(w, h);
                self.height = h as usize;
                // Deterministic reflow: blank the viewport and replay the
                // whole transcript at the new width (a stale copy may
                // remain in the terminal's own scrollback above, like any
                // CLI's output after a resize).
                term.clear_viewport()?;
                self.state.reflow(w as usize);
            }
            _ => {}
        }
        Ok(Flow::Continue)
    }

    /// Enter: slash commands dispatch immediately (even mid-run — they act
    /// on the session, not the transcript); otherwise start a run when
    /// idle, or queue steering when one is active (delivered at the next
    /// turn boundary, pi-style).
    fn submit(&mut self, text: String) {
        if let Some((name, args)) = parse_slash(text.trim()) {
            self.state.push_user(text.trim());
            self.dispatch_command(name, args);
            return;
        }
        self.state.push_user(&text);
        if self.run_cancel.is_some() {
            self.steering
                .lock()
                .expect("steering queue")
                .push_back(text);
            return;
        }
        let mut ctx = self.ctx.take().expect("context is home while idle");
        // Snapshot before the prompt; the prompt + the run's output is the
        // session-log delta (PLAN.md WS4.1).
        let start_len = ctx.messages.len();
        ctx.messages.push(Message::User(UserMessage::new(text)));
        self.start_run(ctx, start_len);
    }

    fn start_run(&mut self, mut ctx: Box<Context>, start_len: usize) {
        let cwd = match std::env::current_dir() {
            Ok(cwd) => cwd,
            Err(e) => {
                self.state
                    .push_line(transcript::error_line(&format!("resolving cwd: {e}"), true));
                self.ctx = Some(ctx);
                return;
            }
        };
        // Rebuilt per run: an extension may have registered tools since.
        ctx.system_prompt = Some(phi_agent::prompt::build(
            &self.spec.host.tools(),
            self.spec.system.as_deref(),
            &cwd,
        ));

        let run_cancel = self.spec.cancel.child_token();
        self.run_cancel = Some(run_cancel.clone());
        self.state.run_started();

        let host = Arc::clone(&self.spec.host);
        let registry = Arc::clone(&self.spec.registry);
        let model = self.spec.model.clone();
        let auth = self.spec.auth.clone();
        let thinking = self.spec.thinking;
        let steering = Arc::clone(&self.steering);
        let tx = self.tx.clone();
        let store = Arc::clone(&self.store);
        tokio::spawn(async move {
            let stream_cancel = run_cancel.clone();
            let mut stream_fn = move |ctx: &Context| {
                let mut opts = StreamOptions::new(auth.clone());
                opts.thinking = thinking;
                opts.cancel = stream_cancel.child_token();
                registry.stream(&model, ctx, opts)
            };
            let mut on_event = |event: AgentEvent| {
                let _ = tx.send(UiMsg::Agent(event));
            };
            let outcome = run_flat(
                &host,
                &mut stream_fn,
                &mut ctx,
                &run_cancel,
                &steering,
                &mut on_event,
            )
            .await;
            // Flush the run's delta to the session log off the UI loop.
            // The transcript stays well-formed even when the run was
            // aborted: the loop appends error/aborted assistant messages.
            crate::session::flush(&store, &ctx.messages, start_len);
            let _ = tx.send(UiMsg::RunDone(ctx, outcome));
        });
    }

    fn on_run_done(&mut self, ctx: Box<Context>, outcome: &Result<RunOutcome>) {
        self.run_cancel = None;
        self.ctx = Some(ctx);
        self.state.run_finished(outcome);
        // Steering that arrived after the loop's final drain never reached
        // the model; carry it into a fresh run so the input isn't lost.
        let leftover: Vec<String> = self
            .steering
            .lock()
            .expect("steering queue")
            .drain(..)
            .collect();
        if !leftover.is_empty() {
            let mut ctx = self.ctx.take().expect("context returned above");
            // Each leftover steering line is its own logged turn; snapshot
            // before pushing so the prompts land in the session log.
            let start_len = ctx.messages.len();
            for text in leftover {
                ctx.messages.push(Message::User(UserMessage::new(text)));
            }
            self.start_run(ctx, start_len);
        }
    }

    /// Invoke on the host off the UI loop — a Lua handler may burn its
    /// whole watchdog window, and the spinner should keep moving.
    fn dispatch_command(&mut self, name: String, args: String) {
        let host = Arc::clone(&self.spec.host);
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let result = host.invoke_command(&name, &args);
            let _ = tx.send(UiMsg::Command { name, result });
        });
    }

    /// Render a command's result: `{ message }` prints as a `•` line;
    /// `{ action }` dispatches on the string key (PLAN.md WS3.3 — an open
    /// vocabulary, never a closed enum). Both may appear together.
    fn on_command_done(
        &mut self,
        name: &str,
        result: Result<Option<Value>>,
        term: &mut Terminal<Stdout>,
    ) -> Result<Flow> {
        let value = match result {
            Ok(Some(value)) => value,
            Ok(None) => return Ok(Flow::Continue),
            Err(e) => {
                self.state
                    .push_line(transcript::error_line(&format!("{e:#}"), true));
                return Ok(Flow::Continue);
            }
        };
        if let Some(message) = value.get("message").and_then(Value::as_str) {
            self.state.push_notice(message);
        }
        if let Some(action) = value.get("action").and_then(Value::as_str) {
            return self.dispatch_action(name, action, &value, term);
        }
        Ok(Flow::Continue)
    }

    fn dispatch_action(
        &mut self,
        command: &str,
        action: &str,
        value: &Value,
        term: &mut Terminal<Stdout>,
    ) -> Result<Flow> {
        let str_arg = |key: &str| value.get(key).and_then(Value::as_str).unwrap_or("");
        match action {
            "quit" => return Ok(Flow::Quit),
            "clear" => {
                if self.run_cancel.is_some() {
                    self.state.push_line(transcript::error_line(
                        "can't clear while a run is active (Esc to interrupt it)",
                        true,
                    ));
                    return Ok(Flow::Continue);
                }
                if let Some(ctx) = self.ctx.as_mut() {
                    ctx.messages.clear();
                }
                self.state.clear_transcript();
                term.clear_viewport()?;
            }
            "set_model" => self.set_model(str_arg("query")),
            "login" => self.login(str_arg("args")),
            "logout" => self.logout(str_arg("args")),
            "open_tree" => self.open_tree(),
            "fork" => self.open_fork_search(),
            other => self.state.push_line(transcript::error_line(
                &format!("/{command}: this frontend has no handler for action '{other}'"),
                true,
            )),
        }
        Ok(Flow::Continue)
    }

    /// Route a key to the active overlay, if any. Returns `Some(Flow)`
    /// when the overlay consumed the key (the caller returns it); `None`
    /// when no overlay is open or the key passed through (a `Tree` overlay
    /// only returns `None` for keys the browser didn't claim, so the editor
    /// still gets them — but pi's tree swallows everything, so we close on
    /// pass-through).
    fn overlay_lines(&self) -> Option<Vec<String>> {
        let overlay = self.overlay.as_ref()?;
        let width = self.state.width();
        match overlay {
            Overlay::Tree(browser) => Some(tree_overlay::render(browser, width)),
            Overlay::Fork(selector) => {
                let (lines, _cursor) = selector.render(width, self.height);
                let mut out = Vec::with_capacity(lines.len() + 2);
                out.push(format!(
                    "{}{}Fork from message{} — Esc to close, Enter to fork",
                    DIM, BOLD, RESET
                ));
                out.extend(lines);
                Some(out)
            }
        }
    }

    fn handle_overlay_key(
        &mut self,
        key: KeyEvent,
        _term: &mut Terminal<Stdout>,
    ) -> Result<Option<Flow>> {
        let Some(overlay) = self.overlay.take() else {
            return Ok(None);
        };
        match overlay {
            Overlay::Tree(mut browser) => {
                if let Some(outcome) = tree_overlay::handle(&mut browser, key) {
                    self.apply_tree_outcome(outcome, _term)?;
                    // `apply_tree_outcome` clears `self.overlay`; a navigate/cc
                    // closes it, a label edit keeps the tree open underneath
                    // (the label editor is not yet wired, so cancel closes).
                } else {
                    // Pass-through key: keep the browser open (it re-renders).
                    self.overlay = Some(Overlay::Tree(browser));
                }
            }
            Overlay::Fork(mut selector) => {
                use phi_tui::SelectorOutcome;
                let outcome = selector.handle_key(key);
                match outcome {
                    SelectorOutcome::Confirmed(idx) => {
                        let value = selector.items()[idx].value.clone();
                        drop(selector);
                        self.fork_to(&value, _term)?;
                    }
                    SelectorOutcome::Cancelled => {
                        // Esc: close, restore the editor.
                    }
                    SelectorOutcome::Pending => {
                        self.overlay = Some(Overlay::Fork(selector));
                    }
                }
            }
        }
        Ok(Some(Flow::Continue))
    }

    /// `/tree` and double-Esc: build the browser from the live session tree
    /// and swap it in as the overlay. A guard: no tree, nothing to browse.
    fn open_tree(&mut self) {
        if self.run_cancel.is_some() {
            self.state.push_line(transcript::error_line(
                "can't open the tree while a run is active (Esc to interrupt it)",
                true,
            ));
            return;
        }
        let (tree, leaf) = {
            let store = self.store.lock().expect("session store");
            match (store.tree(), store.leaf_id()) {
                (Ok(t), l) => (t, l),
                (Err(e), _) => {
                    self.state.push_line(transcript::error_line(
                        &format!("session tree: {e:#}"),
                        true,
                    ));
                    return;
                }
            }
        };
        if tree.is_empty() {
            self.state.push_notice("nothing to browse yet");
            return;
        }
        let height = self.height;
        self.overlay = Some(Overlay::Tree(Box::new(tree_overlay::browser(
            &tree,
            leaf.as_deref(),
            height,
        ))));
    }

    /// `/fork` selector: list user messages. Builds the items from the live
    /// session tree (oldest-to-newest) and preselects the latest.
    fn open_fork_search(&mut self) {
        if self.run_cancel.is_some() {
            self.state.push_line(transcript::error_line(
                "can't fork while a run is active (Esc to interrupt it)",
                true,
            ));
            return;
        }
        let items = self.user_messages_for_fork();
        if items.is_empty() {
            self.state.push_notice("no user messages to fork from");
            return;
        }
        // Preselect the most recent (last) user message, like pi.
        let last_value = items.last().map(|i| i.value.clone());
        let items = items
            .into_iter()
            .map(|i| phi_tui::SelectorItem::new(i.value, i.label).detail(i.detail))
            .collect();
        let mut selector = phi_tui::Selector::new("Fork from message", items, true);
        if let Some(v) = last_value {
            selector.preselect(&v);
        }
        self.overlay = Some(Overlay::Fork(selector));
    }

    /// User messages along the *current leaf path* (root → leaf), in order.
    /// Forks always run off the active branch, mirroring pi's
    /// `getUserMessagesForForking` which iterates `getEntries` — but phi's
    /// entries are a tree, so we walk the leaf path (the live transcript)
    /// and return only user messages with non-empty text.
    fn user_messages_for_fork(&self) -> Vec<ForkItem> {
        let store = self.store.lock().expect("session store");
        let Ok(parsed) = phi_session::read_tree(store.path()) else {
            return Vec::new();
        };
        let path = phi_session::leaf_path(&parsed.tree, parsed.leaf_id.as_deref());
        path.into_iter()
            .enumerate()
            .filter_map(|(i, node)| {
                let phi_ai::Message::User(u) = &node.message else {
                    return None;
                };
                let text = u.content.trim();
                if text.is_empty() {
                    return None;
                }
                Some(ForkItem {
                    value: node.fields.id.clone(),
                    label: text.chars().take(80).collect(),
                    detail: format!("#{} of {}", i + 1, path_len_hint(&parsed.tree)),
                })
            })
            .collect()
    }

    /// Apply a tree-browser outcome: navigate the leaf in place, open the
    /// label editor, or close. Navigating rebuilds the transcript from the
    /// new leaf path and re-renders the whole viewport (pi's
    /// `renderInitialMessages` after `navigateTree`).
    fn apply_tree_outcome(
        &mut self,
        outcome: tree_overlay::TreeOutcome,
        term: &mut Terminal<Stdout>,
    ) -> Result<()> {
        match outcome {
            tree_overlay::TreeOutcome::Navigate(entry_id) => {
                self.navigate_leaf(&entry_id, term)?;
            }
            tree_overlay::TreeOutcome::Label { entry_id, label } => {
                self.set_label(&entry_id, label);
            }
            tree_overlay::TreeOutcome::Cancel => {}
        }
        self.overlay = None;
        Ok(())
    }

    /// `branch(to_id)` + rebuild the transcript from the new leaf path,
    /// blanking the viewport first so old lines don't linger (pi's
    /// `renderInitialMessages`). A no-op-ish content guard: branching to
    /// the current leaf is rejected by the store (same leaf id).
    fn navigate_leaf(&mut self, entry_id: &str, term: &mut Terminal<Stdout>) -> Result<()> {
        let messages = {
            let mut store = self.store.lock().expect("session store");
            if let Err(e) = store.branch(Some(entry_id)) {
                self.state
                    .push_line(transcript::error_line(&format!("navigate: {e:#}"), true));
                return Ok(());
            }
            match store.leaf_messages() {
                Ok(m) => m,
                Err(e) => {
                    self.state.push_line(transcript::error_line(
                        &format!("reloading transcript: {e:#}"),
                        true,
                    ));
                    return Ok(());
                }
            }
        };
        self.state.clear_transcript();
        term.clear_viewport()?;
        // Re-render the messages as if they had streamed.
        for message in messages {
            self.state.push_message_replay(&message);
        }
        // The live `Context` must match the new leaf so the next append
        // / run continues from here; a branch shortens it.
        if let Some(ctx) = self.ctx.as_mut() {
            ctx.messages.clear();
            if let Some(system) = ctx.system_prompt.take() {
                ctx.system_prompt = Some(system);
            }
        }
        // Re-load into the owned context without clobbering system_prompt.
        if let Some(ctx) = self.ctx.as_mut() {
            let system = ctx.system_prompt.take();
            let mut fresh = Box::<Context>::default();
            fresh.system_prompt = system;
            {
                let store = self.store.lock().expect("session store");
                if let Ok(parsed) = phi_session::read_tree(store.path()) {
                    fresh.messages = phi_session::messages_along_leaf_path(
                        &parsed.tree,
                        parsed.leaf_id.as_deref(),
                    );
                }
            }
            *ctx = fresh;
        }
        self.state.push_notice("navigated to selected point");
        Ok(())
    }

    /// Fork: create a branched session off `from_entry_id`'s message path,
    /// swap the store, and seed the editor with that user message's text for
    /// re-editing. A new-session file means the transcript starts fresh.
    fn fork_to(&mut self, from_entry_id: &str, _term: &mut Terminal<Stdout>) -> Result<()> {
        // Up to the message (inclusive): fork the leaf at that message.
        let (new_store, editor_text) = {
            let store = self.store.lock().expect("session store");
            match store.create_branched_session(Some(from_entry_id)) {
                Ok(new_store) => {
                    // Find the message text to seed the editor with (pi's
                    // `extractUserMessageText`).
                    let text = match store.tree() {
                        Ok(tree) => phi_session::find_node(&tree, from_entry_id)
                            .and_then(|n| match &n.message {
                                phi_ai::Message::User(u) => Some(u.content.clone()),
                                _ => None,
                            })
                            .unwrap_or_default(),
                        Err(_) => String::new(),
                    };
                    (new_store, text)
                }
                Err(e) => {
                    self.state
                        .push_line(transcript::error_line(&format!("fork: {e:#}"), true));
                    return Ok(());
                }
            }
        };
        let parsed = phi_session::read_tree(new_store.path())
            .map(|p| phi_session::messages_along_leaf_path(&p.tree, p.leaf_id.as_deref()))
            .unwrap_or_default();
        // Swap the store: the old session file stays on disk untouched.
        {
            let mut store = self.store.lock().expect("session store");
            *store = new_store;
        }
        self.state.clear_transcript();
        _term.clear_viewport()?;
        for message in parsed {
            self.state.push_message_replay(&message);
        }
        // Rebuild the live context for the forked session.
        if let Some(ctx) = self.ctx.as_mut() {
            let system = ctx.system_prompt.take();
            let mut fresh = Box::<Context>::default();
            fresh.system_prompt = system;
            fresh.messages = {
                let store = self.store.lock().expect("session store");
                phi_session::read_tree(store.path())
                    .map(|p| phi_session::messages_along_leaf_path(&p.tree, p.leaf_id.as_deref()))
                    .unwrap_or_default()
            };
            *ctx = fresh;
        }
        // Seed the editor with the forked message's text (pi's
        // `editor.setText(result.selectedText)`).
        self.state.editor.set_text(&editor_text);
        self.state
            .slash_completions
            .sync_from_editor(self.state.editor.before_cursor());
        self.state.push_notice(&format!(
            "forked to new session{}",
            if editor_text.is_empty() {
                ""
            } else {
                " (edit and resend)"
            }
        ));
        Ok(())
    }

    /// `/label`: set or clear a label on an entry (pi's label editor; here a
    /// single command sets it — the interactive editor lands later).
    fn set_label(&mut self, entry_id: &str, label: Option<String>) {
        let mut store = self.store.lock().expect("session store");
        match store.label(entry_id, label.as_deref()) {
            Ok(_) => {
                let tag = label
                    .as_deref()
                    .map(|l| format!("labeled: {l}"))
                    .unwrap_or_else(|| "label cleared".to_owned());
                let _ = tag;
            }
            Err(e) => self
                .state
                .push_line(transcript::error_line(&format!("label: {e:#}"), true)),
        }
    }

    /// `/model` — show the current model, or switch. The switch lands via
    /// `ModelChanged` once the new provider's auth resolves (resolution may
    /// refresh a token, so it runs off the UI loop); until then the old
    /// model stays active.
    fn set_model(&mut self, query: &str) {
        let query = query.trim();
        if query.is_empty() {
            self.state.push_notice(&format!(
                "model: {}/{}\nswitch: /model <id>   (ids: phi --list-models)",
                self.spec.model.provider, self.spec.model.id
            ));
            return;
        }
        let Some(model) = self.spec.registry.find(query) else {
            self.state.push_line(transcript::error_line(
                &format!("unknown model '{query}' (see phi --list-models)"),
                true,
            ));
            return;
        };
        if !self.spec.registry.is_supported(model) {
            self.state.push_line(transcript::error_line(
                &format!(
                    "no provider implementation for '{}/{}'",
                    model.provider, model.id
                ),
                true,
            ));
            return;
        }
        let model = model.clone();
        let storage = Arc::clone(&self.spec.storage);
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let msg = match storage.resolve(&model.provider).await {
                Ok(Some(auth)) => UiMsg::ModelChanged { model, auth },
                Ok(None) => UiMsg::CommandFailed(format!(
                    "no credentials for provider '{}' (try /login {})",
                    model.provider, model.provider
                )),
                Err(e) => UiMsg::CommandFailed(format!(
                    "auth resolution failed for '{}': {e}",
                    model.provider
                )),
            };
            let _ = tx.send(msg);
        });
    }

    /// `/login` — the same OAuth flow as `--login`, with its prompts
    /// rendered as `•` notices instead of stdout prints (raw mode stays
    /// on; the browser callback server completes the flow).
    fn login(&mut self, args: &str) {
        if self.login_cancel.is_some() {
            self.state.push_line(transcript::error_line(
                "a login is already in progress (Esc to cancel it)",
                true,
            ));
            return;
        }
        let mut words = args.split_whitespace();
        let provider = match words.next() {
            None => {
                self.state
                    .push_notice("usage: /login anthropic|openai [device]");
                return;
            }
            Some(word) => match OAuthProviderId::parse(word) {
                Some(provider) => provider,
                None => {
                    self.state.push_line(transcript::error_line(
                        &format!("unknown provider '{word}' (anthropic | openai)"),
                        true,
                    ));
                    return;
                }
            },
        };
        let method = match words.next() {
            None => LoginMethod::Browser,
            Some("device") => LoginMethod::DeviceCode,
            Some(other) => {
                self.state.push_line(transcript::error_line(
                    &format!("unknown login method '{other}' (only: device)"),
                    true,
                ));
                return;
            }
        };
        let cancel = self.spec.cancel.child_token();
        self.login_cancel = Some(cancel.clone());
        let storage = Arc::clone(&self.spec.storage);
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let callbacks = NoticeCallbacks { tx: tx.clone() };
            let result = storage
                .login(provider, method, &callbacks, &cancel)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string());
            let _ = tx.send(UiMsg::LoginDone { provider, result });
        });
    }

    fn on_login_done(&mut self, provider: OAuthProviderId, result: &Result<(), String>) {
        self.login_cancel = None;
        match result {
            Ok(()) => {
                self.state
                    .push_notice(&format!("logged in to {}", provider.storage_key()));
                // Fresh credentials for the active model take effect on the
                // next run; a failure here just leaves the old auth in place.
                if self.spec.model.provider == provider.storage_key() {
                    let storage = Arc::clone(&self.spec.storage);
                    let provider = self.spec.model.provider.clone();
                    let tx = self.tx.clone();
                    tokio::spawn(async move {
                        if let Ok(Some(auth)) = storage.resolve(&provider).await {
                            let _ = tx.send(UiMsg::AuthRefreshed(auth));
                        }
                    });
                }
            }
            Err(e) => self
                .state
                .push_line(transcript::error_line(&format!("login failed: {e}"), true)),
        }
    }

    fn logout(&mut self, args: &str) {
        let Some(provider) = OAuthProviderId::parse(args.trim()) else {
            self.state.push_notice("usage: /logout anthropic|openai");
            return;
        };
        match self.spec.storage.logout(provider.storage_key()) {
            Ok(()) => self
                .state
                .push_notice(&format!("logged out of {}", provider.storage_key())),
            Err(e) => self
                .state
                .push_line(transcript::error_line(&format!("logout failed: {e}"), true)),
        }
    }
}

/// `/name [args]` when the first token is exactly `/` + one word of
/// `[A-Za-z0-9_-]`. Anything else — `/etc/hosts`, a lone `/`, `//` — is a
/// regular prompt for the model.
fn parse_slash(text: &str) -> Option<(String, String)> {
    let rest = text.strip_prefix('/')?;
    let (name, args) = match rest.split_once(char::is_whitespace) {
        Some((name, args)) => (name, args),
        None => (rest, ""),
    };
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    Some((name.to_owned(), args.trim().to_owned()))
}

/// Maximum candidate rows shown in the slash completion popup.
const SLASH_COMPLETION_LIMIT: usize = 7;

/// Inline `/command` completion popup (PLAN.md WS3): mirrors jcode's
/// `SlashSuggestionState` but sources candidates dynamically from the
/// host's live command registry (`Host::commands()`), so user/extensions
/// commands appear automatically — no hand-maintained table to drift from.
///
/// Lifecycle: while the editor's text (trimmed) is `/prefix` with no
/// whitespace, the popup shows prefix-then-fuzzy matches. Esc dismisses it
/// until the next key changes the query; Tab/Enter complete the prefix;
/// Up/Down move the selection (preempting the editor's history navigation).
struct SlashCompletions {
    /// `("/name", Option<description>)` for every host-registered command,
    /// snapshot once at startup (commands don't change mid-session).
    all: Vec<(String, Option<String>)>,
    visible: bool,
    dismissed_query: Option<String>,
    selected: usize,
    /// Matches for the current query, ordered prefix-first then fuzzy.
    candidates: Vec<(String, Option<String>)>,
}

impl SlashCompletions {
    fn from_host(host: &Host) -> Self {
        let all: Vec<(String, Option<String>)> = host
            .commands()
            .into_iter()
            // Ambiguity would let one name shadow another; the host's
            // duplicate-registration guard already prevents this.
            .map(|c| (format!("/{}", c.name), c.description))
            .collect();
        Self::new(all)
    }

    fn new(mut all: Vec<(String, Option<String>)>) -> Self {
        all.sort_by(|a, b| a.0.cmp(&b.0));
        Self {
            all,
            visible: false,
            dismissed_query: None,
            selected: 0,
            candidates: Vec::new(),
        }
    }

    fn is_open(&self) -> bool {
        self.visible && !self.candidates.is_empty()
    }

    /// Filter `all` by `query` (the `/prefix` without leading slash is fine
    /// either way): exact prefix matches first, then fuzzy. Re-selects the
    /// first item so the popup always has a valid focus on a fresh query.
    fn recompute(&mut self, query: &str) {
        let needle = query.trim_start_matches('/').to_lowercase();
        let needle = needle.as_str();
        let mut prefix_matches: Vec<(String, Option<String>)> = Vec::new();
        let mut fuzzy: Vec<(f64, (String, Option<String>))> = Vec::new();
        for cmd in &self.all {
            let name_lower = cmd.0.to_lowercase();
            if needle.is_empty() {
                prefix_matches.push(cmd.clone());
            } else if name_lower.starts_with(&format!("/{}", needle)) {
                prefix_matches.push(cmd.clone());
            } else if let Some(score) = fuzzy_match(needle, &cmd.0) {
                fuzzy.push((score, cmd.clone()));
            }
        }
        fuzzy.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut combined = prefix_matches;
        combined.extend(fuzzy.into_iter().map(|(_, c)| c));
        combined.truncate(SLASH_COMPLETION_LIMIT);
        self.candidates = combined;
        self.selected = 0;
        self.visible = !needle.is_empty() && !self.candidates.is_empty();
    }

    /// Sync with the editor: show the popup when the draft trims to a bare
    /// `/word` (no whitespace — a space means args are being typed, so
    /// completion is no longer useful). Re-arms after Esc dismissal as soon
    /// as the query changes.
    fn sync_from_editor(&mut self, before_cursor: &str) {
        // Mirror jcode: a bare `/word` (no whitespace anywhere — a trailing
        // space means args are being typed) shows the popup; anything else
        // hides it. `trim_start` aligns indentation without clearing the
        // signal that args are in progress. Re-arms after Esc dismissal as
        // soon as the query changes.
        let draft = before_cursor.trim_start();
        match parse_slash(draft) {
            // `parse_slash` returns `Some` only for a single `/word` token
            // with no whitespace, reusing the exact charset the dispatcher
            // accepts — so a completed `/mo`→`/model` will dispatch.
            Some((_, _)) => {
                let prefix = draft.to_owned();
                if self.dismissed_query.as_deref() == Some(&prefix) {
                    return;
                }
                self.dismissed_query = None;
                self.recompute(&prefix);
            }
            None => {
                self.visible = false;
                self.candidates.clear();
            }
        }
    }

    /// Cycle selection within the popup; returns `true` if consumed.
    fn move_selection(&mut self, delta: i32) -> bool {
        if !self.is_open() {
            return false;
        }
        let len = self.candidates.len();
        if len == 0 {
            return false;
        }
        let idx = self.selected as i32 + delta;
        self.selected = idx.rem_euclid(len as i32) as usize;
        true
    }

    /// Replace the `/prefix` before the cursor with the selected candidate,
    /// leaving the cursor right after the completed `/name` (so args can
    /// be typed or Enter can fire immediately afterwards).
    fn complete_selected(&mut self, editor: &mut Editor) {
        let Some(candidate) = self.candidates.get(self.selected).cloned() else {
            return;
        };
        let before = editor.before_cursor();
        let prefix = before
            .trim()
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_owned();
        if prefix.starts_with('/') && !prefix.is_empty() {
            editor.replace_prefix(&prefix, &candidate.0);
        }
    }

    /// Dismiss until the query changes (Esc semantics).
    fn dismiss(&mut self, current_query: &str) {
        self.visible = false;
        self.dismissed_query = Some(current_query.trim().to_owned());
    }

    /// Lines to render above the editor (dim title + candidate rows).
    /// Returns an empty vec when hidden.
    fn render_lines(&self) -> Vec<String> {
        if !self.is_open() {
            return Vec::new();
        }
        let mut lines = vec![dim("commands")];
        for (i, (name, desc)) in self.candidates.iter().enumerate() {
            let label = if i == self.selected {
                if let Some(desc) = desc {
                    format!(
                        "{CYAN}{FOCUS}{RESET} {BOLD}{}{RESET} {DIM}{}{RESET}",
                        name, desc
                    )
                } else {
                    format!("{CYAN}{FOCUS}{RESET} {BOLD}{}{RESET}", name)
                }
            } else if let Some(desc) = desc {
                format!("  {} {DIM}{}{RESET}", name, desc)
            } else {
                format!("  {}", name)
            };
            lines.push(label);
        }
        lines
    }
}

/// Login-flow prompts as `•` notices in the transcript (the chat frontend's
/// stand-in for `--login`'s stdout prints).
struct NoticeCallbacks {
    tx: mpsc::UnboundedSender<UiMsg>,
}

impl LoginCallbacks for NoticeCallbacks {
    fn on_auth_url(&self, url: &str, instructions: &str) {
        let _ = self
            .tx
            .send(UiMsg::Notice(format!("{instructions}\n{url}")));
    }

    fn on_device_code(&self, user_code: &str, verification_uri: &str, expires_in: Duration) {
        let _ = self.tx.send(UiMsg::Notice(format!(
            "visit {verification_uri} and enter code {user_code} (expires in {} minutes)",
            expires_in.as_secs() / 60
        )));
    }

    fn on_progress(&self, message: &str) {
        let _ = self.tx.send(UiMsg::Notice(message.to_owned()));
    }
}

/// One committed transcript unit. Blocks are retained so a resize can
/// replay the whole transcript at the new width.
enum Block {
    /// User prompt, echoed as typed under the `❯` marker.
    User(String),
    /// A completed assistant markdown block.
    Markdown(String),
    /// A completed reasoning trace, shown dim under a `• thinking` header.
    Thinking(String),
    /// A pre-styled status line (`•` tool / error / retry lines).
    Line(String),
    Blank,
}

fn block_lines(block: &Block, width: usize, palette: &MarkdownPalette) -> Vec<String> {
    match block {
        Block::User(text) => text.lines().enumerate().map(prefix_prompt).collect(),
        Block::Markdown(text) => render_markdown(text, width, palette),
        Block::Thinking(text) => transcript::thinking_block(text, true),
        Block::Line(line) => vec![line.clone()],
        Block::Blank => vec![String::new()],
    }
}

fn prefix_prompt((i, line): (usize, &str)) -> String {
    if i == 0 {
        format!("{PROMPT} {line}")
    } else {
        format!("  {line}")
    }
}

struct ChatState {
    editor: Editor,
    /// Everything committed to scrollback, in source form.
    transcript: Vec<Block>,
    /// Rendered lines committed but not yet written to the terminal.
    pending: Vec<String>,
    /// Text of the in-progress assistant block (live preview; committed by
    /// `TextEnd`, or by `run_finished` when the stream dies mid-block).
    stream_text: String,
    /// Text of the in-progress reasoning trace (live preview; committed by
    /// `ThinkingEnd`, or by `run_finished`/`TextStart` when interrupted).
    stream_thinking: String,
    /// Whether to render reasoning traces at all. Toggled with Ctrl-T
    /// (pi's `app.thinking.toggle`); a model still thinks and burns the
    /// tokens — this only hides the trace from the transcript.
    show_thinking: bool,
    running: bool,
    started: Instant,
    usage: Usage,
    model_name: String,
    palette: MarkdownPalette,
    width: usize,
    /// Show tool output even when it succeeded (PLAN.md WS3.2).
    verbose: bool,
    /// Inline `/command` completion popup (PLAN.md WS3).
    slash_completions: SlashCompletions,
}

impl ChatState {
    fn new(model_name: String, width: usize, host: &Host) -> Self {
        Self {
            editor: Editor::new(),
            transcript: Vec::new(),
            pending: Vec::new(),
            stream_text: String::new(),
            stream_thinking: String::new(),
            show_thinking: true,
            running: false,
            started: Instant::now(),
            usage: Usage::default(),
            model_name,
            palette: MarkdownPalette::default_const(),
            width: width.max(1),
            verbose: false,
            slash_completions: SlashCompletions::from_host(host),
        }
    }

    fn elapsed_ms(&self) -> u64 {
        if self.running {
            self.started.elapsed().as_millis() as u64
        } else {
            0
        }
    }

    fn take_pending(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pending)
    }

    fn run_started(&mut self) {
        self.running = true;
        self.started = Instant::now();
        self.stream_text.clear();
        self.stream_thinking.clear();
    }

    fn run_finished(&mut self, outcome: &Result<RunOutcome>) {
        self.running = false;
        if self.show_thinking && !self.stream_thinking.is_empty() {
            let text = std::mem::take(&mut self.stream_text);
            let thinking = std::mem::take(&mut self.stream_thinking);
            self.push_thinking(&thinking);
            if !text.is_empty() {
                self.push_markdown(&text);
            }
        } else if !self.stream_text.is_empty() {
            let text = std::mem::take(&mut self.stream_text);
            self.push_markdown(&text);
        } else {
            // Hidden: drop any partial trace; the text flush handles itself.
            self.stream_thinking.clear();
        }
        match outcome {
            Ok(o) if matches!(o.stop_reason, StopReason::Aborted) => {
                self.push_line(transcript::interrupted_line(true));
            }
            Ok(o) if o.is_error() => {
                let msg = o
                    .error_message
                    .as_deref()
                    .unwrap_or("unknown provider error");
                self.push_line(transcript::error_line(msg, true));
            }
            Ok(_) => {}
            Err(e) => self.push_line(transcript::error_line(&format!("{e:#}"), true)),
        }
    }

    fn on_agent_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Assistant(AssistantEvent::TextStart { .. }) => {
                self.stream_text.clear();
                // Text follows thinking: commit any partial trace.
                if self.show_thinking && !self.stream_thinking.is_empty() {
                    let thinking = std::mem::take(&mut self.stream_thinking);
                    self.push_thinking(&thinking);
                } else {
                    self.stream_thinking.clear();
                }
            }
            AgentEvent::Assistant(AssistantEvent::TextDelta { delta, .. }) => {
                self.stream_text.push_str(&delta);
            }
            AgentEvent::Assistant(AssistantEvent::TextEnd { content, .. }) => {
                self.stream_text.clear();
                if self.show_thinking && !self.stream_thinking.is_empty() {
                    let thinking = std::mem::take(&mut self.stream_thinking);
                    self.push_thinking(&thinking);
                } else {
                    self.stream_thinking.clear();
                }
                self.push_markdown(&content);
            }
            AgentEvent::Assistant(AssistantEvent::ThinkingStart { .. }) => {
                self.stream_thinking.clear();
            }
            AgentEvent::Assistant(AssistantEvent::ThinkingDelta { delta, .. }) => {
                if self.show_thinking {
                    self.stream_thinking.push_str(&delta);
                }
            }
            AgentEvent::Assistant(AssistantEvent::ThinkingEnd { content, .. }) => {
                self.stream_thinking.clear();
                if self.show_thinking {
                    self.push_thinking(&content);
                }
            }
            AgentEvent::Assistant(AssistantEvent::RetryRollback { attempt, max }) => {
                self.stream_text.clear();
                self.stream_thinking.clear();
                self.push_line(transcript::retry_line(attempt, max, true));
            }
            AgentEvent::Assistant(
                AssistantEvent::Done { message } | AssistantEvent::Error { message },
            ) => {
                self.usage.add(&message.usage);
            }
            AgentEvent::ToolStart {
                name, arguments, ..
            } => {
                self.push_line(transcript::tool_line(&name, &arguments, true));
            }
            AgentEvent::ToolEnd {
                content, is_error, ..
            } => {
                if is_error {
                    self.push_line(transcript::error_line(&content, true));
                }
                if self.verbose {
                    for line in transcript::output_block(&content, true) {
                        self.push_line(line);
                    }
                }
            }
            _ => {}
        }
    }

    /// The live region: in-progress markdown preview, spinner + dim
    /// elapsed while a turn runs, the slash-completion popup (when open),
    /// the editor, and one dim status line.
    fn width(&self) -> usize {
        self.width
    }

    /// The live region, optionally replaced by an overlay's lines. When an
    /// overlay is `Some`, it owns the region and the editor cursor is hidden
    /// (pi's selectors take focus). Otherwise the editor + popup + status
    /// render as usual.
    fn live_lines_with_overlay(
        &self,
        elapsed_ms: u64,
        overlay: Option<&[String]>,
    ) -> (Vec<String>, Option<LiveCursor>) {
        if let Some(lines) = overlay {
            return (lines.to_vec(), None);
        }
        let (lines, cursor) = self.live_lines_inner(elapsed_ms);
        (lines, Some(cursor))
    }

    fn live_lines(&self, elapsed_ms: u64) -> (Vec<String>, LiveCursor) {
        let (lines, cursor) = self.live_lines_with_overlay(elapsed_ms, None);
        (lines, cursor.expect("non-overlay path always has a cursor"))
    }

    fn live_lines_inner(&self, elapsed_ms: u64) -> (Vec<String>, LiveCursor) {
        let mut lines = Vec::new();
        if self.running {
            if self.show_thinking && !self.stream_thinking.is_empty() {
                for line in transcript::thinking_block(&self.stream_thinking, true) {
                    lines.push(line);
                }
            }
            if !self.stream_text.is_empty() {
                lines.extend(render_markdown(
                    &self.stream_text,
                    self.width,
                    &self.palette,
                ));
            }
            lines.push(format!(
                "{} {}",
                spinner_frame_at(elapsed_ms),
                dim(&format!("{}s", elapsed_ms / 1000))
            ));
            lines.push(String::new());
        }
        // Slash-completion popup: one dim title + candidate rows, above
        // the editor. The cursor stays on the editor line, so these extra
        // lines only shift the offset, not the column.
        let popup_lines = self.slash_completions.render_lines();
        let popup_len = popup_lines.len();
        lines.extend(popup_lines);
        if popup_len > 0 {
            lines.push(String::new());
        }
        let (editor_lines, (row, col)) = self.editor.render();
        let offset = lines.len();
        for (i, line) in editor_lines.iter().enumerate() {
            lines.push(prefix_prompt((i, line)));
        }
        let cursor = LiveCursor {
            line: offset + row,
            col: 2 + visible_width(&editor_lines[row][..col]),
        };
        lines.push(dim(&self.status_line()));
        (lines, cursor)
    }

    fn status_line(&self) -> String {
        format!(
            "{} · {} tokens · ${:.4}",
            self.model_name,
            format_tokens(self.usage.total_tokens),
            self.usage.cost.total
        )
    }

    /// Re-render the whole transcript at a new width (the caller blanked
    /// the viewport; the replay arrives as scrollback on the next frame).
    fn reflow(&mut self, width: usize) {
        self.width = width.max(1);
        self.pending = self
            .transcript
            .iter()
            .flat_map(|block| block_lines(block, self.width, &self.palette))
            .collect();
    }

    fn push_block(&mut self, block: Block) {
        self.pending
            .extend(block_lines(&block, self.width, &self.palette));
        self.transcript.push(block);
    }

    fn push_user(&mut self, text: &str) {
        if !self.transcript.is_empty() {
            self.push_block(Block::Blank);
        }
        self.push_block(Block::User(text.to_owned()));
    }

    /// Replay a transcript message after a branch/fork/rebuild (pi's
    /// `renderInitialMessages`). Users render under `❯` (typed); assistants
    /// as markdown; tool results and bare tool calls as dim `•` lines so a
    /// restored session reads like one that streamed.
    fn push_message_replay(&mut self, message: &Message) {
        match message {
            Message::User(u) => self.push_user(&u.content),
            Message::Assistant(a) => {
                let mut text_parts = Vec::new();
                for block in &a.content {
                    if let phi_ai::AssistantContent::Text { text } = block {
                        text_parts.push(text.clone());
                    }
                }
                if !text_parts.is_empty() {
                    self.push_markdown(&text_parts.join("\n"));
                }
            }
            Message::ToolResult(r) => {
                self.push_line(transcript::tool_line(
                    "tool",
                    &serde_json::json!({ "tool_use_id": r.tool_use_id }),
                    true,
                ));
            }
        }
    }

    fn push_markdown(&mut self, text: &str) {
        self.push_block(Block::Blank);
        self.push_block(Block::Markdown(text.trim_end().to_owned()));
    }

    /// A completed reasoning trace: dim `• thinking` header + head-capped
    /// content. Groups like a `•` line.
    fn push_thinking(&mut self, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        self.push_block(Block::Blank);
        self.push_block(Block::Thinking(text.trim_end().to_owned()));
    }

    /// `•` lines group tightly: a blank only when leaving another kind.
    fn push_line(&mut self, line: String) {
        if !matches!(self.transcript.last(), Some(Block::Line(_))) {
            self.push_block(Block::Blank);
        }
        self.push_block(Block::Line(line));
    }

    /// A (possibly multi-line) command reply as one dim `•` block.
    fn push_notice(&mut self, text: &str) {
        for line in transcript::notice_lines(text, true) {
            self.push_line(line);
        }
    }

    /// `/clear`: drop the committed transcript and anything pending; the
    /// caller blanks the viewport (scrollback already written to the
    /// terminal stays in its history, as with a resize replay).
    fn clear_transcript(&mut self) {
        self.transcript.clear();
        self.pending.clear();
    }
}

fn dim(s: &str) -> String {
    transcript::dim(s, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use phi_tui::strip_ansi;

    fn state() -> ChatState {
        state_with_commands(Vec::new())
    }

    fn state_with_commands(cmds: Vec<(&str, Option<&str>)>) -> ChatState {
        let all = cmds
            .into_iter()
            .map(|(n, d)| (format!("/{}", n), d.map(str::to_owned)))
            .collect();
        let mut s = ChatState::new("test-model".to_owned(), 40, &Host::new().unwrap());
        s.slash_completions = SlashCompletions::new(all);
        s
    }

    #[test]
    fn idle_live_region_is_editor_plus_status() {
        let mut s = state();
        s.editor.insert_str("hi");
        let (lines, cursor) = s.live_lines(0);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "❯ hi");
        assert_eq!(strip_ansi(&lines[1]), "test-model · 0 tokens · $0.0000");
        assert_eq!(cursor, LiveCursor { line: 0, col: 4 });
    }

    #[test]
    fn cursor_column_counts_display_width() {
        let mut s = state();
        s.editor.insert_str("é日"); // 1 + 2 display columns
        let (_, cursor) = s.live_lines(0);
        assert_eq!(cursor.col, 2 + 3);
    }

    #[test]
    fn multiline_editor_offsets_cursor_line() {
        let mut s = state();
        s.editor.insert_str("one\ntwo");
        let (lines, cursor) = s.live_lines(0);
        assert_eq!(lines[0], "❯ one");
        assert_eq!(lines[1], "  two");
        assert_eq!(cursor, LiveCursor { line: 1, col: 5 });
    }

    #[test]
    fn running_live_region_has_spinner_preview_and_editor() {
        let mut s = state();
        s.run_started();
        s.stream_text = "hello".to_owned();
        let (lines, cursor) = s.live_lines(3_200);
        // Preview, spinner, blank, editor, status.
        assert_eq!(lines.len(), 5);
        assert_eq!(strip_ansi(&lines[0]).trim(), "hello");
        assert!(strip_ansi(&lines[1]).contains("3s"));
        assert_eq!(strip_ansi(&lines[3]), "❯ ");
        assert_eq!(cursor.line, 3);
    }

    #[test]
    fn user_prompts_flush_as_typed_with_blank_separation() {
        let mut s = state();
        s.push_user("first");
        s.push_user("second\nline");
        let flushed = s.take_pending();
        assert_eq!(flushed, vec!["❯ first", "", "❯ second", "  line"]);
        assert!(s.take_pending().is_empty());
    }

    #[test]
    fn markdown_blocks_flush_after_a_blank() {
        let mut s = state();
        s.push_user("hi");
        s.on_agent_event(AgentEvent::Assistant(AssistantEvent::TextEnd {
            index: 0,
            content: "hello there\n".to_owned(),
        }));
        let flushed = s.take_pending();
        assert_eq!(flushed[0], "❯ hi");
        assert_eq!(flushed[1], "");
        assert!(
            flushed[2..]
                .iter()
                .any(|l| strip_ansi(l).contains("hello there"))
        );
    }

    #[test]
    fn tool_lines_group_without_blanks() {
        let mut s = state();
        s.on_agent_event(AgentEvent::ToolStart {
            id: "1".into(),
            name: "bash".into(),
            arguments: serde_json::json!({ "command": "ls" }),
        });
        s.on_agent_event(AgentEvent::ToolEnd {
            id: "1".into(),
            name: "bash".into(),
            content: "boom\nmore".into(),
            is_error: true,
        });
        let flushed = s.take_pending();
        let plain: Vec<String> = flushed.iter().map(|l| strip_ansi(l)).collect();
        assert_eq!(plain, vec!["", "• bash ls", "• ERROR: boom"]);
    }

    #[test]
    fn verbose_shows_output_of_successful_tools() {
        let mut s = state();
        s.verbose = true;
        s.on_agent_event(AgentEvent::ToolStart {
            id: "1".into(),
            name: "bash".into(),
            arguments: serde_json::json!({ "command": "ls" }),
        });
        s.on_agent_event(AgentEvent::ToolEnd {
            id: "1".into(),
            name: "bash".into(),
            content: "a.txt\nb.txt".into(),
            is_error: false,
        });
        let plain: Vec<String> = s.take_pending().iter().map(|l| strip_ansi(l)).collect();
        assert_eq!(plain, vec!["", "• bash ls", "  a.txt", "  b.txt"]);
    }

    #[test]
    fn streaming_deltas_accumulate_and_commit_on_text_end() {
        let mut s = state();
        s.run_started();
        for delta in ["hel", "lo"] {
            s.on_agent_event(AgentEvent::Assistant(AssistantEvent::TextDelta {
                index: 0,
                delta: delta.to_owned(),
            }));
        }
        assert_eq!(s.stream_text, "hello");
        s.on_agent_event(AgentEvent::Assistant(AssistantEvent::TextEnd {
            index: 0,
            content: "hello".to_owned(),
        }));
        assert!(s.stream_text.is_empty());
        assert!(!s.take_pending().is_empty());
    }

    #[test]
    fn aborted_run_flushes_partial_text_and_interrupt_line() {
        let mut s = state();
        s.run_started();
        s.stream_text = "partial answer".to_owned();
        s.run_finished(&Ok(RunOutcome {
            stop_reason: StopReason::Aborted,
            error_message: None,
            usage: Usage::default(),
            turns: 1,
        }));
        assert!(!s.running);
        let plain: Vec<String> = s.take_pending().iter().map(|l| strip_ansi(l)).collect();
        assert!(plain.iter().any(|l| l.contains("partial answer")));
        assert!(plain.iter().any(|l| l.contains("• interrupted")));
    }

    #[test]
    fn provider_error_flushes_error_line() {
        let mut s = state();
        s.run_started();
        s.run_finished(&Ok(RunOutcome {
            stop_reason: StopReason::Error,
            error_message: Some("overloaded".to_owned()),
            usage: Usage::default(),
            turns: 1,
        }));
        let plain: Vec<String> = s.take_pending().iter().map(|l| strip_ansi(l)).collect();
        assert!(plain.iter().any(|l| l.contains("• ERROR: overloaded")));
    }

    #[test]
    fn reflow_replays_the_whole_transcript() {
        let mut s = state();
        s.push_user("hi");
        s.push_markdown("a paragraph of text that will wrap differently");
        let first = s.take_pending();
        s.reflow(20);
        let replay = s.take_pending();
        assert!(replay.len() > first.len()); // narrower width → more rows
        assert_eq!(replay[0], "❯ hi");
    }

    #[test]
    fn slash_parses_only_single_token_commands() {
        assert_eq!(parse_slash("/quit"), Some(("quit".into(), "".into())));
        assert_eq!(
            parse_slash("/model claude-opus-4-7"),
            Some(("model".into(), "claude-opus-4-7".into()))
        );
        assert_eq!(
            parse_slash("/login openai device"),
            Some(("login".into(), "openai device".into()))
        );
        // Paths, bare slashes, and plain prose go to the model.
        assert_eq!(parse_slash("/etc/hosts what is this"), None);
        assert_eq!(parse_slash("/"), None);
        assert_eq!(parse_slash("// a comment"), None);
        assert_eq!(parse_slash("hello"), None);
    }

    #[test]
    fn notice_block_groups_like_tool_lines() {
        let mut s = state();
        s.push_user("/help");
        s.push_notice("commands:\n/quit — quit phi");
        let plain: Vec<String> = s.take_pending().iter().map(|l| strip_ansi(l)).collect();
        assert_eq!(
            plain,
            vec!["❯ /help", "", "• commands:", "  /quit — quit phi"]
        );
    }

    #[test]
    fn clear_drops_transcript_and_pending() {
        let mut s = state();
        s.push_user("hi");
        s.push_markdown("answer");
        s.clear_transcript();
        assert!(s.take_pending().is_empty());
        s.reflow(40); // replays the transcript — which must now be empty
        assert!(s.take_pending().is_empty());
    }

    #[test]
    fn retry_clears_partial_stream_text() {
        let mut s = state();
        s.run_started();
        s.stream_text = "half".to_owned();
        s.on_agent_event(AgentEvent::Assistant(AssistantEvent::RetryRollback {
            attempt: 1,
            max: 3,
        }));
        assert!(s.stream_text.is_empty());
        let plain: Vec<String> = s.take_pending().iter().map(|l| strip_ansi(l)).collect();
        assert!(plain.iter().any(|l| l.contains("retrying (1/3)")));
    }

    #[test]
    fn thinking_deltas_accumulate_and_commit_on_thinking_end() {
        let mut s = state();
        s.run_started();
        s.on_agent_event(AgentEvent::Assistant(AssistantEvent::ThinkingStart {
            index: 0,
        }));
        for delta in ["one", " two"] {
            s.on_agent_event(AgentEvent::Assistant(AssistantEvent::ThinkingDelta {
                index: 0,
                delta: delta.to_owned(),
            }));
        }
        assert_eq!(s.stream_thinking, "one two");
        // While streaming, the trace shows in the live region.
        let (lines, _) = s.live_lines(0);
        let plain: Vec<String> = lines.iter().map(|l| strip_ansi(l)).collect();
        assert!(plain.iter().any(|l| l.contains("• thinking")));
        assert!(plain.iter().any(|l| l.contains("one two")));

        s.on_agent_event(AgentEvent::Assistant(AssistantEvent::ThinkingEnd {
            index: 0,
            content: "one two".to_owned(),
        }));
        assert!(s.stream_thinking.is_empty());
        // Committed to scrollback as a `• thinking` block.
        let plain: Vec<String> = s.take_pending().iter().map(|l| strip_ansi(l)).collect();
        assert!(plain.iter().any(|l| l == "• thinking"));
        assert!(plain.iter().any(|l| l == "  one two"));
    }

    #[test]
    fn trace_without_end_is_flushed_when_text_starts() {
        // A model that streams thinking then dies mid-trace: the partial
        // trace must still be committed before text (and on run_finished).
        let mut s = state();
        s.run_started();
        s.on_agent_event(AgentEvent::Assistant(AssistantEvent::ThinkingDelta {
            index: 0,
            delta: "partial trace".to_owned(),
        }));
        s.on_agent_event(AgentEvent::Assistant(AssistantEvent::TextStart {
            index: 0,
        }));
        assert!(s.stream_thinking.is_empty());
        let plain: Vec<String> = s.take_pending().iter().map(|l| strip_ansi(l)).collect();
        assert!(plain.iter().any(|l| l == "• thinking"));
        assert!(plain.iter().any(|l| l.contains("partial trace")));
    }

    #[test]
    fn aborted_run_flushes_partial_thinking() {
        let mut s = state();
        s.run_started();
        s.stream_thinking = "half a trace".to_owned();
        s.run_finished(&Ok(RunOutcome {
            stop_reason: StopReason::Aborted,
            error_message: None,
            usage: Usage::default(),
            turns: 1,
        }));
        assert!(s.stream_thinking.is_empty());
        let plain: Vec<String> = s.take_pending().iter().map(|l| strip_ansi(l)).collect();
        assert!(plain.iter().any(|l| l == "• thinking"));
        assert!(plain.iter().any(|l| l.contains("half a trace")));
        assert!(plain.iter().any(|l| l.contains("• interrupted")));
    }

    #[test]
    fn toggle_hides_thinking_trace() {
        // Ctrl-T flips `show_thinking`; when off, no trace is accumulated,
        // committed, or shown in the live region. The model still thinks —
        // this is display-only.
        let mut s = state();
        s.run_started();
        assert!(s.show_thinking);
        s.show_thinking = false;
        s.on_agent_event(AgentEvent::Assistant(AssistantEvent::ThinkingStart {
            index: 0,
        }));
        s.on_agent_event(AgentEvent::Assistant(AssistantEvent::ThinkingDelta {
            index: 0,
            delta: "hidden".to_owned(),
        }));
        assert!(
            s.stream_thinking.is_empty(),
            "hidden: buffer not accumulated"
        );
        s.on_agent_event(AgentEvent::Assistant(AssistantEvent::ThinkingEnd {
            index: 0,
            content: "hidden".to_owned(),
        }));
        let plain: Vec<String> = s.take_pending().iter().map(|l| strip_ansi(l)).collect();
        assert!(!plain.iter().any(|l| l.contains("hidden")));
        assert!(!plain.iter().any(|l| l == "• thinking"));

        // Flipping back on resumes rendering.
        s.show_thinking = true;
        s.on_agent_event(AgentEvent::Assistant(AssistantEvent::ThinkingDelta {
            index: 0,
            delta: "back".to_owned(),
        }));
        let (lines, _) = s.live_lines(0);
        let plain: Vec<String> = lines.iter().map(|l| strip_ansi(l)).collect();
        assert!(plain.iter().any(|l| l.contains("back")));
    }

    fn completions(cmds: Vec<(&str, Option<&str>)>) -> SlashCompletions {
        let all = cmds
            .into_iter()
            .map(|(n, d)| (format!("/{}", n), d.map(str::to_owned)))
            .collect();
        SlashCompletions::new(all)
    }

    #[test]
    fn slash_completions_open_on_prefix() {
        let mut sc = completions(vec![
            ("model", Some("switch the model")),
            ("quit", None),
            ("clear", None),
            ("commands", None),
        ]);
        sc.sync_from_editor("/c");
        assert!(sc.is_open());
        // Prefix matches first: /clear, /commands (sorted), /c is not a
        // command so there is no exact match.
        let names: Vec<&str> = sc.candidates.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["/clear", "/commands"]); // prefix-sorted
    }

    #[test]
    fn slash_completions_prefix_beats_fuzzy() {
        let mut sc = completions(vec![("commands", None), ("clear", None), ("model", None)]);
        sc.sync_from_editor("/c");
        // /clear and /commands are prefix matches; /model is not, even
        // though "c" appears in "model" as a subsequence.
        let names: Vec<&str> = sc.candidates.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.starts_with(&["/clear", "/commands"]));
        assert!(!names.contains(&"/model"));
    }

    #[test]
    fn slash_completions_fuzzy_fills_beyond_prefix() {
        let mut sc = completions(vec![
            ("model", None),
            ("login", None),
            ("ml", None), // exact prefix match for "ml"
        ]);
        sc.sync_from_editor("/ml");
        // /ml is the exact prefix match → first. /model is a fuzzy fill:
        // not a prefix of "ml", but "ml" is a subsequence of "model".
        assert_eq!(sc.candidates[0].0, "/ml");
        assert!(sc.candidates.iter().any(|(n, _)| n == "/model"));
    }

    #[test]
    fn slash_completions_hidden_after_space() {
        let mut sc = completions(vec![("model", None)]);
        sc.sync_from_editor("/model ");
        assert!(!sc.is_open());
        assert!(sc.candidates.is_empty());
    }

    #[test]
    fn slash_completions_hidden_for_non_slash() {
        let mut sc = completions(vec![("model", None)]);
        sc.sync_from_editor("hello");
        assert!(!sc.is_open());
        sc.sync_from_editor("/etc/hosts");
        assert!(!sc.is_open());
    }

    #[test]
    fn slash_completions_esc_dismisses_until_change() {
        let mut sc = completions(vec![("model", None)]);
        sc.sync_from_editor("/mo");
        assert!(sc.is_open());
        sc.dismiss("/mo");
        assert!(!sc.is_open());
        // Same query after dismiss → stays hidden.
        sc.sync_from_editor("/mo");
        assert!(!sc.is_open());
        // Typing more re-arms.
        sc.sync_from_editor("/mod");
        assert!(sc.is_open());
    }

    #[test]
    fn slash_completions_tab_completes_prefix() {
        let mut sc = completions(vec![("model", Some("switch the model"))]);
        let mut ed = Editor::new();
        ed.insert_str("/mo");
        sc.sync_from_editor(ed.before_cursor());
        assert!(sc.is_open());
        sc.complete_selected(&mut ed);
        assert_eq!(ed.text(), "/model");
    }

    #[test]
    fn slash_completions_render_lines_when_open() {
        let mut sc = completions(vec![("model", Some("switch")), ("quit", None)]);
        sc.sync_from_editor("/m");
        let lines = sc.render_lines();
        assert!(lines.len() >= 2); // title + at least one candidate
        let plain: Vec<String> = lines.iter().map(|l| strip_ansi(l)).collect();
        // Title line.
        assert_eq!(plain[0], "commands");
        // Selected candidate (first) has the focus marker.
        assert!(plain[1].starts_with("▌"));
        assert!(plain[1].contains("/model"));
    }

    #[test]
    fn slash_completions_render_lines_empty_when_closed() {
        let sc = completions(vec![("model", None)]);
        assert!(sc.render_lines().is_empty());
    }

    #[test]
    fn live_region_includes_popup_when_open() {
        let mut s = state_with_commands(vec![("model", None), ("quit", None)]);
        s.editor.insert_str("/m");
        s.slash_completions
            .sync_from_editor(s.editor.before_cursor());
        let (lines, cursor) = s.live_lines(0);
        // popup: "commands" + 1 candidate(say /model) + blank + editor ❯ /m + status
        let plain: Vec<String> = lines.iter().map(|l| strip_ansi(l)).collect();
        assert!(plain.iter().any(|l| l == "commands"));
        assert!(plain.iter().any(|l| l.contains("/model")));
        // Editor + status still present.
        assert!(plain.iter().any(|l| l.starts_with("❯ /m")));
        assert!(plain.iter().any(|l| l.contains("test-model · 0 tokens")));
        // Cursor is on the editor line.
        assert_eq!(cursor.col, 4); // "❯ /m" → col after "❯ /m"
    }
}
