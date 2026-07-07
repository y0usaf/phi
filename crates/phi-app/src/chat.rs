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
    AssistantEvent, Auth, Context, Message, Model, Registry, StopReason, StreamOptions,
    ThinkingLevel, Usage, UserMessage,
};
use phi_host::Host;
use phi_tui::crossterm::event::{Event, KeyCode, KeyEventKind};
use phi_tui::{
    Editor, EditorAction, LiveCursor, MarkdownPalette, PROMPT, RawModeGuard, SPINNER_FRAME_MS,
    Terminal, render_markdown, spinner_frame_at, visible_width,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::agent::summarize;
use crate::format::{format_tokens, format_usage_summary};

pub struct ChatSpec {
    pub host: Arc<Host>,
    pub registry: Arc<Registry>,
    pub model: Model,
    pub auth: Auth,
    pub thinking: ThinkingLevel,
    /// Replaces the prompt head; project context + date/cwd tail stays.
    pub system: Option<String>,
    /// Session-level cancellation (external SIGINT): quits the frontend and
    /// winds down any active run (run tokens are children of this one).
    pub cancel: CancellationToken,
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

    let mut chat = Chat::new(spec, tx, width as usize);
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
}

impl Chat {
    fn new(spec: ChatSpec, tx: mpsc::UnboundedSender<UiMsg>, width: usize) -> Self {
        let state = ChatState::new(spec.model.id.clone(), width);
        Self {
            spec,
            tx,
            state,
            steering: Arc::default(),
            ctx: Some(Box::default()),
            run_cancel: None,
        }
    }

    fn handle(&mut self, msg: UiMsg, term: &mut Terminal<Stdout>) -> Result<Flow> {
        match msg {
            UiMsg::Input(event) => return self.handle_input(event, term),
            UiMsg::Agent(event) => self.state.on_agent_event(event),
            UiMsg::RunDone(ctx, outcome) => self.on_run_done(ctx, &outcome),
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

    /// Enter: start a run when idle; queue steering when one is active
    /// (delivered at the next turn boundary, pi-style).
    fn submit(&mut self, text: String) {
        self.state.push_user(&text);
        if self.run_cancel.is_some() {
            self.steering
                .lock()
                .expect("steering queue")
                .push_back(text);
            return;
        }
        let mut ctx = self.ctx.take().expect("context is home while idle");
        ctx.messages.push(Message::User(UserMessage::new(text)));
        self.start_run(ctx);
    }

    fn start_run(&mut self, mut ctx: Box<Context>) {
        let cwd = match std::env::current_dir() {
            Ok(cwd) => cwd,
            Err(e) => {
                self.state
                    .push_line(red(&format!("● ERROR: resolving cwd: {e}")));
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
            for text in leftover {
                ctx.messages.push(Message::User(UserMessage::new(text)));
            }
            self.start_run(ctx);
        }
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
                self.push_line(dim("● interrupted"));
            }
            Ok(o) if o.is_error() => {
                let msg = o
                    .error_message
                    .as_deref()
                    .unwrap_or("unknown provider error");
                self.push_line(red(&format!("● ERROR: {msg}")));
            }
            Ok(_) => {}
            Err(e) => self.push_line(red(&format!("● ERROR: {e:#}"))),
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
                self.push_line(dim(&format!("● retrying ({attempt}/{max})")));
            }
            AgentEvent::Assistant(
                AssistantEvent::Done { message } | AssistantEvent::Error { message },
            ) => {
                self.usage.add(&message.usage);
            }
            AgentEvent::ToolStart {
                name, arguments, ..
            } => {
                self.push_line(format!("● {name} {}", dim(&summarize(&arguments))));
            }
            AgentEvent::ToolEnd {
                content,
                is_error: true,
                ..
            } => {
                let first = content.lines().next().unwrap_or("").trim();
                self.push_line(red(&format!("● ERROR: {first}")));
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
}

fn dim(s: &str) -> String {
    format!("\x1b[2m{s}\x1b[0m")
}

fn red(s: &str) -> String {
    format!("\x1b[31m{s}\x1b[0m")
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
