use std::collections::{HashMap, HashSet};
use std::io::{IsTerminal, Read};
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use phi_ai::{AuthStorage, Registry, ThinkingLevel};
use phi_builtins::prelude::{CODING_PRELUDE, coding_tools_section};
use phi_cli::Args;
use phi_harness::PhiHarness;
use phi_rlm::{ContextPayload, ProviderLm, Rlm, RlmConfig, RlmEvent, StopCause};
use phi_session::config;
use phi_session_tui::session_builder;
use tokio_util::sync::CancellationToken;

use crate::format::format_usage_summary;
use crate::render::render_tool_panel;
use crate::tool_view::{call_anchor, check, cross, depth_tag, indent, preview, spin};

/// Run a single prompt to completion (non-TUI mode).
pub async fn run(
    args: Args,
    storage: Arc<AuthStorage>,
    registry: Arc<Registry>,
    cwd: std::path::PathBuf,
) -> Result<()> {
    let prompt = args.prompt_text();
    let ndjson = args.output.as_deref() == Some("ndjson");

    let settings = config::Settings::resolve(
        &args.to_session_args(),
        config::Config::load(&cwd)?,
        false,
    );

    let root_model = session_builder::find_model(&registry, &settings.model)?;
    let sub_model = match &settings.sub_model {
        Some(query) => session_builder::find_model(&registry, query)?,
        None => root_model.clone(),
    };
    let thinking: ThinkingLevel = settings
        .thinking
        .parse()
        .map_err(|e: String| anyhow::anyhow!(e))?;
    let sub_model_thinking: ThinkingLevel = settings
        .sub_model_thinking
        .parse()
        .map_err(|e: String| anyhow::anyhow!(e))?;

    for provider in [&root_model.provider, &sub_model.provider] {
        if storage.resolve(provider).await?.is_none() {
            bail!(
                "no credentials for provider '{}': run `phi --login {}` \
                 or set {}_API_KEY",
                provider,
                provider,
                provider.to_uppercase()
            );
        }
    }

    let skills = phi_session::skills::discover_skills(&cwd, &settings.skills);

    let config = RlmConfig {
        max_rlm_depth: settings.max_rlm_depth,
        max_iterations: settings.max_iterations,
        max_concurrent_subcalls: settings.max_concurrent_subcalls,
        setup_code: Some(CODING_PRELUDE.to_owned()),
        tools_section: Some(coding_tools_section()),
        cwd: Some(cwd.display().to_string()),
        user_prologue: Some(format!(
            "You are a coding agent operating directly in the user's working directory: {}. \
             Use the pre-defined helper functions to explore and modify the project.",
            cwd.display()
        )),
        skills,
        ..RlmConfig::default()
    };

    let root = Arc::new(ProviderLm::new(
        registry.clone(),
        root_model,
        storage.clone(),
        thinking,
        settings.max_tokens,
    ));
    let sub = Arc::new(ProviderLm::new(
        registry.clone(),
        sub_model,
        storage.clone(),
        sub_model_thinking,
        settings.max_tokens,
    ));

    let (payload, root_prompt) = match &args.context {
        Some(source) => (ContextPayload::Str(read_context(source)?), prompt.clone()),
        None => match prompt.clone() {
            Some(text) => (ContextPayload::Str(text), None),
            None => {
                bail!("provide a prompt and/or --context (or run on a TTY for the interactive TUI)")
            }
        },
    };

    let mut rlm = Rlm::new(config, root, sub, settings.python.clone());
    session_builder::register_supported_named_models(
        &mut rlm,
        registry.clone(),
        storage.clone(),
        sub_model_thinking,
    );
    let mut printer = None;
    if !args.quiet {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        rlm = rlm.with_events(tx);
        if ndjson {
            printer = Some(tokio::spawn(print_events_ndjson(rx)));
        } else {
            printer = Some(tokio::spawn(print_events(
                rx,
                args.verbose,
                settings.max_iterations,
            )));
        }
    }

    let cancel = CancellationToken::new();
    let ctrl_c_cancel = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("\ninterrupted, finishing up...");
            ctrl_c_cancel.cancel();
        }
    });

    let harness = PhiHarness::new(rlm);
    let result = harness
        .prompt(payload, root_prompt.as_deref(), &cancel)
        .await?;
    drop(harness);
    if let Some(printer) = printer {
        let _ = printer.await;
    }

    if ndjson {
        let result_json = serde_json::json!({
            "type": "result",
            "response": result.response,
            "stop": result.stop,
            "iterations": result.iterations,
            "duration": result.duration.as_secs_f64(),
        });
        println!("{}", result_json);
    } else {
        println!("{}", result.response);
        if !args.quiet {
            eprintln!(
                "\n[{:?}] {} iteration(s), {:.1}s",
                result.stop,
                result.iterations,
                result.duration.as_secs_f64(),
            );
            eprintln!("{}", format_usage_summary(&result.usage));
        }
    }
    if result.stop == StopCause::Failed {
        bail!(result.error.unwrap_or_else(|| "run failed".to_owned()));
    }
    Ok(())
}

fn read_context(source: &str) -> Result<String> {
    if source == "-" {
        let mut buffer = String::new();
        std::io::stdin()
            .read_to_string(&mut buffer)
            .context("failed to read context from stdin")?;
        return Ok(buffer);
    }
    std::fs::read_to_string(source).with_context(|| format!("failed to read context: {source}"))
}

/// Remove stdout lines that are verbatim echoes of tool-call results already
/// rendered as panels above (e.g. `print(grep(...))`). This is a **data-level**
/// coalescing of the stdout stream — applied before printing — never a layout
/// deletion. Returns the residual stdout; when empty, the caller skips
/// printing the duplicate output.
fn cli_strip_tool_echoes(stdout: &str, tool_results: &[String]) -> String {
    let mut result_lines: HashSet<&str> = HashSet::new();
    for r in tool_results {
        for line in r.lines() {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                result_lines.insert(trimmed);
            }
        }
    }
    stdout
        .lines()
        .filter(|line| !result_lines.contains(line.trim()))
        .collect::<Vec<&str>>()
        .join("\n")
}

async fn print_events(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<RlmEvent>,
    verbose: bool,
    max_iterations: usize,
) {
    let color = std::io::stderr().is_terminal();
    let mut open_tool_args: HashMap<u64, serde_json::Value> = HashMap::new();
    let mut tool_results: Vec<String> = Vec::new();
    while let Some(event) = rx.recv().await {
        match event {
            RlmEvent::IterationStart { depth, iteration } => {
                if depth == 0 {
                    eprintln!("\n[turn {}]", iteration + 1);
                } else {
                    eprintln!(
                        "\n[turn {}/{max_iterations}]{}",
                        iteration + 1,
                        depth_tag(depth)
                    );
                }
            }
            RlmEvent::CodeExecStart { code, .. } => {
                tool_results.clear();
                eprintln!("{} exec", spin(color));
                eprintln!("{}", indent(&preview(&code, 16, verbose)));
            }
            RlmEvent::CodeExecEnd {
                stdout,
                stderr,
                duration,
                ..
            } => {
                let glyph = if stderr.trim().is_empty() {
                    check(color)
                } else {
                    cross(color)
                };
                if (duration.as_secs_f64() * 10.0).round() <= 0.0 {
                    eprintln!("  {glyph} exec");
                } else {
                    eprintln!("  {glyph} exec • {:.1}s", duration.as_secs_f64());
                }
                let filtered = cli_strip_tool_echoes(&stdout, &tool_results);
                if !filtered.trim().is_empty() {
                    eprintln!("{}", indent(&preview(&filtered, 12, verbose)));
                }
                if !stderr.trim().is_empty() {
                    eprintln!("{}", indent(&preview(&stderr, 12, verbose)));
                }
            }
            RlmEvent::SubcallStart {
                depth,
                model,
                prompt_preview,
            } => {
                eprintln!(
                    "{} subcall {model}{} {prompt_preview}",
                    spin(color),
                    depth_tag(depth)
                );
            }
            RlmEvent::SubcallEnd {
                model,
                duration,
                error,
                ..
            } => match error {
                Some(error) => {
                    eprintln!("  {} subcall {model} failed: {error}", cross(color))
                }
                None => eprintln!(
                    "  {} subcall {model} {:.1}s",
                    check(color),
                    duration.as_secs_f64()
                ),
            },
            RlmEvent::ToolStart {
                call_id,
                depth,
                name,
                args,
            } => {
                open_tool_args.insert(call_id, args.clone());
                eprintln!("{} {}{}", spin(color), call_anchor(&name, &args), depth_tag(depth));
            }
            RlmEvent::ToolEnd {
                call_id,
                name,
                result,
                error,
                duration: _duration,
                ..
            } => {
                let args = open_tool_args
                    .remove(&call_id)
                    .unwrap_or(serde_json::Value::Null);
                if error.is_none() {
                    tool_results.push(result.clone());
                }
                eprintln!(
                    "{}",
                    render_tool_panel(
                        &name,
                        &args,
                        error.as_deref().unwrap_or(&result),
                        error.is_some(),
                        verbose,
                        color
                    )
                );
            }
            RlmEvent::AnswerSubmitted { .. } => {
                eprintln!("{} answer submitted", check(color))
            }
            RlmEvent::RunStart { .. }
            | RlmEvent::RunEnd { .. }
            | RlmEvent::IterationEnd { .. }
            | RlmEvent::RootDelta { .. }
            | RlmEvent::RootThinkingDelta { .. }
            | RlmEvent::RootText { .. }
            | RlmEvent::LimitHit { .. } => {}
        }
    }
}

/// NDJSON event printer: emits each event as a line-delimited JSON object on
/// stdout for machine-readable consumption.
async fn print_events_ndjson(mut rx: tokio::sync::mpsc::UnboundedReceiver<RlmEvent>) {
    while let Some(event) = rx.recv().await {
        if let Ok(line) = serde_json::to_string(&event) {
            println!("{line}");
        }
    }
}
