//! One-shot driver for the agent loop (PLAN.md WS2.4). TTY: streamed text
//! on stdout, one `● name summary` line per tool call on stderr, output
//! suppressed unless it failed (forgecode's vocabulary, minimally). Piped
//! stdout: final text only (pi's print mode).

use std::io::{IsTerminal as _, Write as _};
use std::sync::Arc;

use anyhow::{Result, bail};
use phi_agent::{AgentEvent, RunOutcome, SteeringQueue, run_flat};
use phi_ai::{
    AssistantEvent, Auth, Context, Message, Model, Registry, StopReason, StreamOptions,
    ThinkingLevel, UserMessage,
};
use phi_host::Host;
use tokio_util::sync::CancellationToken;

use crate::format::format_usage_summary;
use crate::transcript;

pub struct RunSpec<'a> {
    pub host: Arc<Host>,
    pub registry: &'a Registry,
    pub model: Model,
    pub auth: Auth,
    pub thinking: ThinkingLevel,
    /// Replaces the prompt head; project context + date/cwd tail stays.
    pub system: Option<String>,
    pub prompt: String,
    /// Show tool output even when it succeeded (PLAN.md WS3.2).
    pub verbose: bool,
    pub cancel: CancellationToken,
}

pub async fn run(spec: RunSpec<'_>) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let system_prompt = phi_agent::prompt::build(&spec.host.tools(), spec.system.as_deref(), &cwd);
    let mut ctx = Context {
        system_prompt: Some(system_prompt),
        messages: vec![Message::User(UserMessage::new(spec.prompt))],
        ..Context::default()
    };

    let mut stream_fn = |ctx: &Context| {
        let mut opts = StreamOptions::new(spec.auth.clone());
        opts.thinking = spec.thinking;
        opts.cancel = spec.cancel.child_token();
        spec.registry.stream(&spec.model, ctx, opts)
    };

    let mut renderer = Renderer::new(std::io::stdout().is_terminal(), spec.verbose);
    // One-shot has no way to inject input mid-run; the queue exists because
    // the loop API demands one (interactive frontends fill it).
    let steering: SteeringQueue = Arc::default();
    let outcome = run_flat(
        &spec.host,
        &mut stream_fn,
        &mut ctx,
        &spec.cancel,
        &steering,
        &mut |event| renderer.on_event(event),
    )
    .await?;
    renderer.finish(&outcome)
}

struct Renderer {
    /// stdout is a terminal: stream text and show tool lines. Otherwise
    /// print mode: final text only.
    interactive: bool,
    /// stderr is a terminal: allowed to color the tool lines.
    color: bool,
    verbose: bool,
    /// stdout cursor sits mid-line (streamed text without a trailing \n).
    line_open: bool,
    final_text: String,
}

impl Renderer {
    fn new(interactive: bool, verbose: bool) -> Self {
        Self {
            interactive,
            color: std::io::stderr().is_terminal(),
            verbose,
            line_open: false,
            final_text: String::new(),
        }
    }

    fn on_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Assistant(AssistantEvent::TextDelta { delta, .. }) if self.interactive => {
                let mut out = std::io::stdout();
                let _ = out.write_all(delta.as_bytes());
                let _ = out.flush();
                self.line_open = !delta.ends_with('\n');
            }
            AgentEvent::Assistant(AssistantEvent::RetryRollback { attempt, max })
                if self.interactive =>
            {
                self.break_line();
                eprintln!("{}", transcript::retry_line(attempt, max, self.color));
            }
            AgentEvent::Assistant(AssistantEvent::Done { message }) => {
                self.final_text = message.text();
            }
            AgentEvent::ToolStart {
                name, arguments, ..
            } if self.interactive => {
                self.break_line();
                eprintln!("{}", transcript::tool_line(&name, &arguments, self.color));
            }
            AgentEvent::ToolEnd {
                content, is_error, ..
            } if self.interactive => {
                if is_error {
                    eprintln!("{}", transcript::error_line(&content, self.color));
                }
                if self.verbose {
                    for line in transcript::output_block(&content, self.color) {
                        eprintln!("{line}");
                    }
                }
            }
            _ => {}
        }
    }

    fn finish(mut self, outcome: &RunOutcome) -> Result<()> {
        self.break_line();
        if !self.interactive && !self.final_text.is_empty() {
            print!("{}", self.final_text);
            if !self.final_text.ends_with('\n') {
                println!();
            }
        }
        eprintln!("{}", format_usage_summary(&outcome.usage));
        if outcome.is_error() {
            match outcome.stop_reason {
                StopReason::Aborted => bail!("aborted"),
                _ => bail!(
                    "provider error: {}",
                    outcome.error_message.as_deref().unwrap_or("unknown error")
                ),
            }
        }
        Ok(())
    }

    fn break_line(&mut self) {
        if self.line_open {
            println!();
            self.line_open = false;
        }
    }
}
