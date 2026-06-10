mod chat;
mod files;
mod session;

use std::io::{IsTerminal, Read};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use phi_ai::{
    AuthStorage, LoginMethod, Model, OAuthProviderId, PrintCallbacks, Registry, ThinkingLevel,
};
use phi_builtins::prelude::{CODING_PRELUDE, coding_tools_section};
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
                println!("{qualified:<40} {}", model.name);
            }
        }
        return Ok(());
    }

    run(args, storage, registry).await
}

async fn run(args: Args, storage: Arc<AuthStorage>, registry: Arc<Registry>) -> Result<()> {
    let root_model = find_model(&registry, &args.model)?;
    let sub_model = match &args.sub_model {
        Some(query) => find_model(&registry, query)?,
        None => root_model.clone(),
    };
    let thinking: ThinkingLevel = args
        .thinking
        .parse()
        .map_err(|e: String| anyhow::anyhow!(e))?;

    if let Some(key) = &args.api_key {
        storage.set_runtime_api_key(&root_model.provider, key);
        if sub_model.provider != root_model.provider {
            storage.set_runtime_api_key(&sub_model.provider, key);
        }
    }
    let prompt = args.prompt_text();
    let chat_mode = prompt.is_none()
        && args.context.is_none()
        && std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal();

    let mut missing_credentials = Vec::new();
    for provider in [&root_model.provider, &sub_model.provider] {
        if !missing_credentials.contains(provider) && storage.resolve(provider).await?.is_none() {
            missing_credentials.push(provider.clone());
        }
    }
    // In chat mode missing credentials are handled in-TUI via /login.
    if !chat_mode && let Some(provider) = missing_credentials.first() {
        bail!(
            "no credentials for provider '{provider}': run `phi --login {provider}`, \
             pass --api-key, or set {}_API_KEY",
            provider.to_uppercase()
        );
    }

    let mut config = RlmConfig {
        max_depth: args.max_depth,
        max_iterations: args.max_iterations,
        max_budget: args.budget,
        max_timeout: args.timeout.map(Duration::from_secs),
        max_tokens: args.max_tokens,
        max_errors: args.max_errors,
        max_concurrent_subcalls: args.max_concurrent_subcalls,
        ..RlmConfig::default()
    };

    let root = Arc::new(ProviderLm::new(
        registry.clone(),
        root_model.clone(),
        storage.clone(),
        thinking,
    ));
    let sub = Arc::new(ProviderLm::new(
        registry.clone(),
        sub_model.clone(),
        storage.clone(),
        ThinkingLevel::Off,
    ));
    let envs = Arc::new(PythonEnvFactory {
        python: args.python.clone(),
    });

    if chat_mode {
        let cwd = std::env::current_dir().context("could not determine working directory")?;
        config.setup_code = Some(CODING_PRELUDE.to_owned());
        config.tools_section = Some(coding_tools_section());
        config.cwd = Some(cwd.display().to_string());
        config.user_prologue = Some(format!(
            "You are a coding agent operating directly in the user's working directory: {}. \
             Use the pre-defined helper functions to explore and modify the project.",
            cwd.display()
        ));
        let payload = ContextPayload::Str(workspace_context(&cwd));
        let store = session::SessionStore::new(&cwd)?;
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let rlm = Rlm::new(config, root.clone(), sub.clone(), envs).with_events(tx);
        return chat::run(chat::ChatSetup {
            rlm,
            payload,
            events: rx,
            missing_credentials,
            ui: chat::UiSetup {
                max_iterations: args.max_iterations,
                storage,
                registry,
                root_lm: root,
                sub_lm: sub,
                store,
            },
        })
        .await;
    }

    let (payload, root_prompt) = match &args.context {
        Some(source) => (ContextPayload::Str(read_context(source)?), prompt.clone()),
        None => match prompt.clone() {
            Some(text) => (ContextPayload::Str(text), None),
            None => bail!("provide a prompt and/or --context (or run on a TTY to chat)"),
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
            args.max_iterations,
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
            "\n[{:?}] {} iteration(s), {:.1}s, {} tokens, ${:.4}",
            result.stop,
            result.iterations,
            result.duration.as_secs_f64(),
            result.usage.total_tokens,
            result.usage.cost.total
        );
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

fn find_model(registry: &Registry, query: &str) -> Result<Model> {
    registry
        .find(query)
        .cloned()
        .with_context(|| format!("unknown model '{query}' (see --list-models)"))
}

fn workspace_context(cwd: &std::path::Path) -> String {
    let mut listing = String::new();
    if let Ok(entries) = std::fs::read_dir(cwd) {
        let mut names: Vec<String> = entries
            .flatten()
            .map(|e| {
                let mut name = e.file_name().to_string_lossy().into_owned();
                if e.path().is_dir() {
                    name.push('/');
                }
                name
            })
            .collect();
        names.sort();
        for name in names.into_iter().take(200) {
            listing.push_str(&name);
            listing.push('\n');
        }
    }
    format!(
        "Top-level directory listing of {}:\n{}",
        cwd.display(),
        listing
    )
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
            RlmEvent::AnswerSubmitted { .. } => eprintln!("  [answer submitted]"),
            RlmEvent::LimitHit { limit, detail, .. } => {
                eprintln!("  [limit {}] {detail}", limit_name(limit));
            }
            RlmEvent::RunStart { .. }
            | RlmEvent::RunEnd { .. }
            | RlmEvent::IterationEnd { .. }
            | RlmEvent::RootDelta { .. }
            | RlmEvent::RootText { .. } => {}
        }
    }
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
