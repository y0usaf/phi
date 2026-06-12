mod config;
mod files;
mod mux;
mod session;
mod session_builder;
mod tui;

use std::io::{IsTerminal, Read};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use phi_ai::{
    AuthStorage, LoginMethod, Model, OAuthProviderId, PrintCallbacks, Registry, ThinkingLevel,
    Usage,
};
use phi_cli::Args;
use phi_rlm::engine::PythonEnvFactory;
use phi_rlm::{ContextPayload, LimitKind, ProviderLm, Rlm, RlmConfig, RlmEvent, StopCause};
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let storage = Arc::new(AuthStorage::new(auth_path()?));

    if let Some(provider) = &args.logout {
        let provider = parse_oauth_provider(provider)?;
        storage.logout(provider.storage_key())?;
        println!("Logged out of {}", provider.storage_key());
        return Ok(());
    }

    if let Some(provider) = &args.login {
        let provider = parse_oauth_provider(provider)?;
        let method = if args.device_code {
            LoginMethod::DeviceCode
        } else {
            LoginMethod::Browser
        };
        storage
            .login(provider, method, &PrintCallbacks, &CancellationToken::new())
            .await?;
        println!("Logged in to {}", provider.storage_key());
        return Ok(());
    }

    let registry = Arc::new(Registry::builtin());

    if let Some(pattern) = &args.list_models {
        for model in registry.supported_models() {
            let qualified = format!("{}/{}", model.provider, model.id);
            if pattern.is_empty() || qualified.contains(pattern.as_str()) {
                println!(
                    "{qualified:<40} {:<28} {}",
                    model.name,
                    format_model_pricing(model)
                );
            }
        }
        return Ok(());
    }

    run(args, storage, registry).await
}

fn format_tokens(count: u64) -> String {
    match count {
        0..=999 => count.to_string(),
        1_000..=9_999 => format!("{:.1}k", count as f64 / 1_000.0),
        10_000..=999_999 => format!("{}k", count / 1_000),
        _ => format!("{:.1}M", count as f64 / 1_000_000.0),
    }
}

fn format_rate(rate: f64) -> String {
    if rate == 0.0 {
        "$0/M".to_owned()
    } else if rate >= 10.0 {
        format!("${rate:.0}/M")
    } else if rate >= 1.0 {
        format!("${rate:.2}/M")
    } else {
        format!("${rate:.4}/M")
    }
}

fn format_model_pricing(model: &Model) -> String {
    let mut parts = vec![
        format!("in {}", format_rate(model.cost.input)),
        format!("out {}", format_rate(model.cost.output)),
    ];
    if model.cost.cache_write > 0.0 {
        parts.push(format!("cache+ {}", format_rate(model.cost.cache_write)));
    }
    if model.cost.cache_read > 0.0 {
        parts.push(format!("cache↺ {}", format_rate(model.cost.cache_read)));
    }
    parts.join(" · ")
}

fn format_usage_summary(usage: &Usage) -> String {
    format!(
        "usage: in {} · out {} · cache+ {} · cache↺ {} · total {} · ${:.4}",
        format_tokens(usage.input),
        format_tokens(usage.output),
        format_tokens(usage.cache_write),
        format_tokens(usage.cache_read),
        format_tokens(usage.total_tokens),
        usage.cost.total
    )
}

async fn run(args: Args, storage: Arc<AuthStorage>, registry: Arc<Registry>) -> Result<()> {
    let cwd = std::env::current_dir().context("could not determine working directory")?;
    let prompt = args.prompt_text();
    let tty = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let tui_mode = prompt.is_none() && args.context.is_none() && tty;

    if args.mux || tui_mode {
        let mux_config = config::Config::load(&cwd)?.mux.unwrap_or_default();
        let mux_enabled =
            args.mux || (!args.no_mux && tui_mode && mux_config.enabled.unwrap_or(false));
        if mux_enabled {
            if !tty {
                bail!("--mux requires an interactive terminal");
            }
            if prompt.is_some() || args.context.is_some() {
                bail!("--mux does not take a prompt or --context");
            }
            let shared = session_builder::SharedResources {
                args,
                storage,
                registry,
            };
            return mux::app::run(shared, mux_config, cwd).await;
        }
    }

    if tui_mode {
        let shared = session_builder::SharedResources {
            args,
            storage,
            registry,
        };
        let setup = session_builder::build_session(&cwd, &shared, None).await?;
        return tui::run(setup, tui::SessionIo::standalone(cwd)).await;
    }

    let settings = config::Settings::resolve(&args, config::Config::load(&cwd)?, false);

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

    if let Some(key) = &args.api_key {
        storage.set_runtime_api_key(&root_model.provider, key);
        if sub_model.provider != root_model.provider {
            storage.set_runtime_api_key(&sub_model.provider, key);
        }
    }
    for provider in [&root_model.provider, &sub_model.provider] {
        if storage.resolve(provider).await?.is_none() {
            bail!(
                "no credentials for provider '{provider}': run `phi --login {provider}`, \
                 pass --api-key, or set {}_API_KEY",
                provider.to_uppercase()
            );
        }
    }

    let config = RlmConfig {
        max_depth: settings.max_depth,
        max_iterations: settings.max_iterations,
        max_budget: settings.budget,
        max_timeout: settings.timeout.map(Duration::from_secs),
        max_tokens: settings.max_tokens,
        max_errors: settings.max_errors,
        max_concurrent_subcalls: settings.max_concurrent_subcalls,
        ..RlmConfig::default()
    };

    let root = Arc::new(ProviderLm::new(
        registry.clone(),
        root_model,
        storage.clone(),
        thinking,
    ));
    let sub = Arc::new(ProviderLm::new(
        registry,
        sub_model,
        storage.clone(),
        sub_model_thinking,
    ));
    let envs = Arc::new(PythonEnvFactory {
        python: settings.python.clone(),
    });

    let (payload, root_prompt) = match &args.context {
        Some(source) => (ContextPayload::Str(read_context(source)?), prompt.clone()),
        None => match prompt.clone() {
            Some(text) => (ContextPayload::Str(text), None),
            None => {
                bail!("provide a prompt and/or --context (or run on a TTY for the interactive TUI)")
            }
        },
    };

    let mut rlm = Rlm::new(config, root, sub, envs);
    let mut printer = None;
    if !args.quiet {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        rlm = rlm.with_events(tx);
        printer = Some(tokio::spawn(print_events(
            rx,
            args.verbose,
            settings.max_iterations,
        )));
    }

    let cancel = CancellationToken::new();
    let ctrl_c_cancel = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("\ninterrupted, finishing up...");
            ctrl_c_cancel.cancel();
        }
    });

    let result = rlm
        .completion(payload, root_prompt.as_deref(), &cancel)
        .await;
    drop(rlm);
    if let Some(printer) = printer {
        let _ = printer.await;
    }

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
    if result.stop == StopCause::Failed {
        bail!(result.error.unwrap_or_else(|| "run failed".to_owned()));
    }
    Ok(())
}

fn auth_path() -> Result<std::path::PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "phi")
        .context("could not determine a home directory")?;
    Ok(dirs.data_dir().join("auth.json"))
}

fn parse_oauth_provider(name: &str) -> Result<OAuthProviderId> {
    OAuthProviderId::parse(name)
        .with_context(|| format!("unknown provider '{name}' (expected: anthropic, openai)"))
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

async fn print_events(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<RlmEvent>,
    verbose: bool,
    max_iterations: usize,
) {
    while let Some(event) = rx.recv().await {
        match event {
            RlmEvent::IterationStart { depth, iteration } => {
                eprintln!(
                    "\n[turn {}/{max_iterations}]{}",
                    iteration + 1,
                    depth_tag(depth)
                );
            }
            RlmEvent::CodeExecStart { code, .. } => {
                eprintln!("{}", indent(&preview(&code, 16, verbose)));
            }
            RlmEvent::CodeExecEnd {
                stdout,
                stderr,
                duration,
                ..
            } => {
                eprintln!("  [exec {:.1}s]", duration.as_secs_f64());
                if !stdout.trim().is_empty() {
                    eprintln!("{}", indent(&preview(&stdout, 12, verbose)));
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
                eprintln!("  [subcall {model}{}] {prompt_preview}", depth_tag(depth));
            }
            RlmEvent::SubcallEnd {
                model,
                duration,
                error,
                ..
            } => match error {
                Some(error) => eprintln!("  [subcall {model} failed: {error}]"),
                None => eprintln!("  [subcall {model} done {:.1}s]", duration.as_secs_f64()),
            },
            RlmEvent::ToolStart {
                depth, name, args, ..
            } => {
                eprintln!(
                    "  [{name}{}] {}",
                    depth_tag(depth),
                    tool_args_preview(&args)
                );
            }
            RlmEvent::ToolEnd {
                name,
                error,
                duration,
                ..
            } => match error {
                Some(error) => eprintln!("  [{name} failed: {}]", preview(&error, 4, verbose)),
                None => eprintln!("  [{name} done {:.1}s]", duration.as_secs_f64()),
            },
            RlmEvent::AnswerSubmitted { .. } => eprintln!("  [answer submitted]"),
            RlmEvent::LimitHit { limit, detail, .. } => {
                eprintln!("  [limit {}] {detail}", limit_name(limit));
            }
            RlmEvent::RunStart { .. }
            | RlmEvent::RunEnd { .. }
            | RlmEvent::IterationEnd { .. }
            | RlmEvent::RootDelta { .. }
            | RlmEvent::RootThinkingDelta { .. }
            | RlmEvent::RootText { .. } => {}
        }
    }
}

/// Compact one-line `key=value` summary of a tool call's arguments.
fn tool_args_preview(args: &serde_json::Value) -> String {
    let Some(map) = args.as_object() else {
        return String::new();
    };
    map.iter()
        .map(|(key, value)| {
            let text = match value {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            let flat = text.replace('\n', "\\n");
            let short: String = flat.chars().take(60).collect();
            let ellipsis = if short.len() < flat.len() { "…" } else { "" };
            format!("{key}={short}{ellipsis}")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn depth_tag(depth: usize) -> String {
    if depth == 0 {
        String::new()
    } else {
        format!(" d{depth}")
    }
}

fn limit_name(limit: LimitKind) -> &'static str {
    match limit {
        LimitKind::Budget => "budget",
        LimitKind::Timeout => "timeout",
        LimitKind::Tokens => "tokens",
        LimitKind::Errors => "errors",
    }
}

fn preview(text: &str, max_lines: usize, verbose: bool) -> String {
    let text = text.trim_end();
    if verbose {
        return text.to_owned();
    }
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= max_lines {
        return text.to_owned();
    }
    let mut out = lines[..max_lines].join("\n");
    out.push_str(&format!("\n... ({} more lines)", lines.len() - max_lines));
    out
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|line| format!("  | {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use phi_ai::{Api, Cost, CostRates};
    use std::collections::BTreeMap;

    fn test_model() -> Model {
        Model {
            id: "m".into(),
            name: "Model".into(),
            api: Api::AnthropicMessages,
            provider: "anthropic".into(),
            base_url: "https://example.test".into(),
            reasoning: false,
            input: vec![],
            cost: CostRates {
                input: 3.0,
                output: 15.0,
                cache_read: 0.3,
                cache_write: 3.75,
            },
            context_window: 200_000,
            max_tokens: 8_192,
            headers: BTreeMap::new(),
            thinking_levels: None,
        }
    }

    #[test]
    fn format_model_pricing_includes_cache_rates_when_present() {
        assert_eq!(
            format_model_pricing(&test_model()),
            "in $3.00/M · out $15/M · cache+ $3.75/M · cache↺ $0.3000/M"
        );
    }

    #[test]
    fn format_usage_summary_shows_breakdown_and_total() {
        let usage = Usage {
            input: 1_200,
            output: 34_000,
            cache_read: 5_600,
            cache_write: 7_800,
            total_tokens: 48_600,
            cost: Cost {
                total: 0.1234,
                ..Cost::default()
            },
        };
        assert_eq!(
            format_usage_summary(&usage),
            "usage: in 1.2k · out 34k · cache+ 7.8k · cache↺ 5.6k · total 48k · $0.1234"
        );
    }
}
