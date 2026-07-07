//! Inline interactive chat frontend (PLAN.md WS3.1) — the first frontend
//! pack. It bridges the agent's event stream to phi-tui's scrollback +
//! live-region terminal; the substrate never renders (DESIGN.md frontend
//! seam). forgecode-minimal chrome: `❯` editor at the bottom (no borders),
//! streamed assistant markdown flushed to scrollback per block, one `●`
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
use phi_tui::crossterm::event::{Event, KeyCode, KeyEventKind};
use phi_tui::{
    Editor, EditorAction, LiveCursor, MarkdownPalette, PROMPT, RawModeGuard, SPINNER_FRAME_MS,
    Terminal, render_markdown, spinner_frame_at, visible_width,
};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::format::{format_tokens, format_usage_summary};
use crate::transcript;

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

    let mut chat = Chat::new(spec, tx, width as usize, ctx, store);
    let mut tick = tokio::time::interval(Duration::from_millis(SPINNER_FRAME_MS));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        let (live, cursor) = chat.state.live_lines(chat.state.elapsed_ms());
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
    /// A dim `●` note from an async command task (login progress etc.).
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
}

impl Chat {
    fn new(
        spec: ChatSpec,
        tx: mpsc::UnboundedSender<UiMsg>,
        width: usize,
        ctx: Box<Context>,
        store: crate::session::SharedStore,
    ) -> Self {
        let mut state = ChatState::new(spec.model.id.clone(), width);
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
                if key.kind != KeyEventKind::Release && key.code == KeyCode::Esc {
                    if let Some(cancel) = &self.run_cancel {
                        cancel.cancel();
                    }
                    if let Some(cancel) = &self.login_cancel {
                        cancel.cancel();
                    }
                    return Ok(Flow::Continue);
                }
                match self.state.editor.handle_key(key) {
                    EditorAction::Submit(text) => self.submit(text),
                    EditorAction::Cancel => {
                        // Ctrl+C: clear a non-empty editor, quit on an
                        // empty one (any active run winds down on exit).
                        if self.state.editor.is_empty() {
                            return Ok(Flow::Quit);
                        }
                        self.state.editor.clear();
                    }
                    EditorAction::Quit => return Ok(Flow::Quit),
                    EditorAction::None => {}
                }
            }
            Event::Paste(text) => self.state.editor.insert_paste(&text),
            Event::Resize(w, h) => {
                term.resize(w, h);
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

    /// Render a command's result: `{ message }` prints as a `●` line;
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
            other => self.state.push_line(transcript::error_line(
                &format!("/{command}: this frontend has no handler for action '{other}'"),
                true,
            )),
        }
        Ok(Flow::Continue)
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
    /// rendered as `●` notices instead of stdout prints (raw mode stays
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

/// Login-flow prompts as `●` notices in the transcript (the chat frontend's
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
    /// A pre-styled status line (`●` tool / error / retry lines).
    Line(String),
    Blank,
}

fn block_lines(block: &Block, width: usize, palette: &MarkdownPalette) -> Vec<String> {
    match block {
        Block::User(text) => text.lines().enumerate().map(prefix_prompt).collect(),
        Block::Markdown(text) => render_markdown(text, width, palette),
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
    running: bool,
    started: Instant,
    usage: Usage,
    model_name: String,
    palette: MarkdownPalette,
    width: usize,
    /// Show tool output even when it succeeded (PLAN.md WS3.2).
    verbose: bool,
}

impl ChatState {
    fn new(model_name: String, width: usize) -> Self {
        Self {
            editor: Editor::new(),
            transcript: Vec::new(),
            pending: Vec::new(),
            stream_text: String::new(),
            running: false,
            started: Instant::now(),
            usage: Usage::default(),
            model_name,
            palette: MarkdownPalette::default_const(),
            width: width.max(1),
            verbose: false,
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
    }

    fn run_finished(&mut self, outcome: &Result<RunOutcome>) {
        self.running = false;
        if !self.stream_text.is_empty() {
            let text = std::mem::take(&mut self.stream_text);
            self.push_markdown(&text);
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
            AgentEvent::Assistant(AssistantEvent::TextStart { .. }) => self.stream_text.clear(),
            AgentEvent::Assistant(AssistantEvent::TextDelta { delta, .. }) => {
                self.stream_text.push_str(&delta);
            }
            AgentEvent::Assistant(AssistantEvent::TextEnd { content, .. }) => {
                self.stream_text.clear();
                self.push_markdown(&content);
            }
            AgentEvent::Assistant(AssistantEvent::RetryRollback { attempt, max }) => {
                self.stream_text.clear();
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
    /// elapsed while a turn runs, the editor, and one dim status line.
    fn live_lines(&self, elapsed_ms: u64) -> (Vec<String>, LiveCursor) {
        let mut lines = Vec::new();
        if self.running {
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

    fn push_markdown(&mut self, text: &str) {
        self.push_block(Block::Blank);
        self.push_block(Block::Markdown(text.trim_end().to_owned()));
    }

    /// `●` lines group tightly: a blank only when leaving another kind.
    fn push_line(&mut self, line: String) {
        if !matches!(self.transcript.last(), Some(Block::Line(_))) {
            self.push_block(Block::Blank);
        }
        self.push_block(Block::Line(line));
    }

    /// A (possibly multi-line) command reply as one dim `●` block.
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
        ChatState::new("test-model".to_owned(), 40)
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
        assert_eq!(plain, vec!["", "● bash ls", "● ERROR: boom"]);
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
        assert_eq!(plain, vec!["", "● bash ls", "  a.txt", "  b.txt"]);
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
        assert!(plain.iter().any(|l| l.contains("● interrupted")));
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
        assert!(plain.iter().any(|l| l.contains("● ERROR: overloaded")));
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
            vec!["❯ /help", "", "● commands:", "  /quit — quit phi"]
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
}
