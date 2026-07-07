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

pub struct RunSpec<'a> {
    pub host: Arc<Host>,
    pub registry: &'a Registry,
    pub model: Model,
    pub auth: Auth,
    pub thinking: ThinkingLevel,
    /// Replaces the prompt head; project context + date/cwd tail stays.
    pub system: Option<String>,
    pub prompt: String,
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

    let mut renderer = Renderer::new(std::io::stdout().is_terminal());
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
    /// stdout cursor sits mid-line (streamed text without a trailing \n).
    line_open: bool,
    final_text: String,
}

impl Renderer {
    fn new(interactive: bool) -> Self {
        Self {
            interactive,
            color: std::io::stderr().is_terminal(),
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
                eprintln!("{}", self.dim(&format!("● retrying ({attempt}/{max})")));
            }
            AgentEvent::Assistant(AssistantEvent::Done { message }) => {
                self.final_text = message.text();
            }
            AgentEvent::ToolStart {
                name, arguments, ..
            } if self.interactive => {
                self.break_line();
                eprintln!("● {name} {}", self.dim(&summarize(&arguments)));
            }
            AgentEvent::ToolEnd {
                content,
                is_error: true,
                ..
            } if self.interactive => {
                let first = content.lines().next().unwrap_or("").trim();
                eprintln!("{}", self.red(&format!("● ERROR: {first}")));
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

    fn dim(&self, s: &str) -> String {
        if self.color {
            format!("\x1b[2m{s}\x1b[0m")
        } else {
            s.to_owned()
        }
    }

    fn red(&self, s: &str) -> String {
        if self.color {
            format!("\x1b[31m{s}\x1b[0m")
        } else {
            s.to_owned()
        }
    }
}

/// One-line argument summary for a `●` tool line: the command or path when
/// the tool has one, compact JSON otherwise, capped for the terminal.
fn summarize(arguments: &serde_json::Value) -> String {
    let text = ["command", "path"]
        .iter()
        .find_map(|k| arguments.get(k).and_then(|v| v.as_str()).map(str::to_owned))
        .unwrap_or_else(|| serde_json::to_string(arguments).unwrap_or_default());
    let line = text.lines().next().unwrap_or("").trim();
    let mut out: String = line.chars().take(80).collect();
    if out.len() < line.len() {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::summarize;
    use serde_json::json;

    #[test]
    fn summary_prefers_command_then_path() {
        assert_eq!(summarize(&json!({ "command": "ls -la" })), "ls -la");
        assert_eq!(summarize(&json!({ "path": "src/main.rs" })), "src/main.rs");
        assert_eq!(summarize(&json!({ "command": "ls", "path": "x" })), "ls");
    }

    #[test]
    fn summary_falls_back_to_json_and_truncates() {
        assert_eq!(summarize(&json!({ "n": 1 })), "{\"n\":1}");
        let long = "x".repeat(200);
        let out = summarize(&json!({ "command": long }));
        assert_eq!(out.chars().count(), 81);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn summary_uses_first_line_only() {
        assert_eq!(summarize(&json!({ "command": "echo a\necho b" })), "echo a");
    }
}
