use std::io::Read;
use std::sync::Arc;

use crate::cli::Args;
use anyhow::{Context as _, Result, bail};
use phi_ai::{AuthStorage, Registry, ThinkingLevel};
use phi_builtins::prelude::{CODING_PRELUDE, coding_tools_section};
use phi_rlm::PhiHarness;
use phi_rlm::{ContextPayload, ProviderLm, Rlm, RlmConfig, StopCause};
use phi_session::config;
use phi_session_tui::session_builder;
use tokio_util::sync::CancellationToken;

use crate::event_print::{print_events, print_events_ndjson};

use crate::format::format_usage_summary;

/// Run a single prompt to completion (non-TUI mode).
pub async fn run(
    args: Args,
    storage: Arc<AuthStorage>,
    registry: Arc<Registry>,
    cwd: std::path::PathBuf,
) -> Result<()> {
    let prompt = args.prompt_text();
    let ndjson = args.output.as_deref() == Some("ndjson");

    let settings =
        config::Settings::resolve(&args.to_session_args(), config::Config::load(&cwd)?, false);

    let thinking: ThinkingLevel = settings
        .thinking
        .parse()
        .map_err(|e: String| anyhow::anyhow!(e))?;
    let sub_model_thinking: ThinkingLevel = settings
        .sub_model_thinking
        .parse()
        .map_err(|e: String| anyhow::anyhow!(e))?;

    // One-shot mode never prompts for project trust; an undecided project
    // simply runs without its .phi/ content.
    let trust = session_builder::resolve_project_trust(&cwd, false);
    if let Some(notice) = &trust.notice {
        eprintln!("phi: {notice}");
    }
    // Same extension runtime as the interactive modes (builtins + Lua), so
    // skills injection, event handlers, and extension-registered providers
    // apply to one-shot runs too. The runtime must boot before model
    // resolution: `--model` may name a bundled-catalog or extension provider.
    let runtime = session_builder::build_runtime(&cwd, &registry, &settings, trust.trusted).await?;
    let (registry, provider_notices) =
        session_builder::registry_with_extension_providers(&registry, &runtime, &storage);
    for notice in &provider_notices {
        eprintln!("phi: {notice}");
    }

    let root_model = session_builder::find_model(&registry, &settings.model)?;
    let sub_model = match &settings.sub_model {
        Some(query) => session_builder::find_model(&registry, query)?,
        None => root_model.clone(),
    };

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

    // `--context` runs are long-context QA payloads: keep the RLM identity
    // and orchestrator addendum they were tuned for. Plain `-p` runs are
    // coding runs (WS9.4): coding identity, no orchestrator addendum — its
    // delegate-by-default stance multiplies latency and degrades output on
    // coding tasks. The old user_prologue restated the identity as a user
    // message; the system prompt now owns it.
    let qa_run = args.context.is_some();
    let config = RlmConfig {
        max_rlm_depth: settings.max_rlm_depth,
        max_iterations: settings.max_iterations,
        max_concurrent_subcalls: settings.max_concurrent_subcalls,
        custom_system_prompt: (!qa_run).then(|| phi_rlm::CODING_IDENTITY.to_owned()),
        setup_code: Some({
            let ext_stubs = runtime.extension_prelude_stubs();
            if ext_stubs.is_empty() {
                CODING_PRELUDE.to_owned()
            } else {
                format!("{CODING_PRELUDE}\n\n# Extension tools\n{ext_stubs}")
            }
        }),
        tools_section: Some(coding_tools_section()),
        cwd: Some(cwd.display().to_string()),
        orchestrator: qa_run,
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
    // Snapshot the tool display metadata before the runtime moves into the
    // engine — the printer task projects tool anchors/categories from it.
    let tool_render = runtime.tool_render_index();
    rlm = rlm.with_runtime(runtime);
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
                settings.tools.clone(),
                tool_render,
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
        // Second Ctrl+C: force-exit. Tokio's signal registration disables the
        // process-default terminate, so without this a stuck stream is
        // unkillable short of SIGKILL.
        tokio::signal::ctrl_c().await.ok();
        eprintln!("\nforced exit (second interrupt)");
        std::process::exit(130);
    });

    let harness = PhiHarness::new(rlm);
    let result = harness
        .prompt(payload, root_prompt.as_deref(), &cancel)
        .await?;
    drop(harness);
    if let Some(printer) = printer {
        // A panic in the printer means events were dropped mid-stream; warn
        // instead of silently finishing.
        if let Err(join_err) = printer.await {
            eprintln!("warning: event printer task failed: {join_err}");
        }
    }

    if ndjson {
        let result_json = serde_json::json!({
            "type": "result",
            "response": result.response,
            "stop": result.stop,
            "iterations": result.iterations,
            "duration": result.duration.as_secs_f64(),
            "usage": result.usage,
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
