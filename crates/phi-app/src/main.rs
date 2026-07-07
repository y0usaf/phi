//! Bare core (DESIGN.md doctrine 06): login, list-models, and a one-shot
//! streamed completion with zero extensions loaded. Everything else arrives
//! as packs on the Lua host.

use std::io::{IsTerminal, Read as _};
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use phi_ai::{AuthStorage, LoginMethod, OAuthProviderId, PrintCallbacks, Registry, ThinkingLevel};
use tokio_util::sync::CancellationToken;

use crate::cli::Args;
use crate::format::format_model_pricing;

mod agent;
mod chat;
mod cli;
mod extensions;
mod format;
mod settings;
mod transcript;

const DEFAULT_MODEL: &str = "claude-opus-4-7";

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

    // No prompt on a terminal means an interactive session; everything
    // else is the one-shot path (pi's print mode when piped).
    let prompt = match args.prompt_text() {
        Some(text) if text == "-" => Some(read_stdin()?),
        Some(text) => Some(text),
        None if !std::io::stdin().is_terminal() => Some(read_stdin()?),
        None if std::io::stdout().is_terminal() => None,
        None => bail!("no prompt given and stdout is not a terminal"),
    };
    if let Some(prompt) = &prompt
        && prompt.trim().is_empty()
    {
        bail!("empty prompt");
    }

    let settings = settings::load();

    let query = args
        .model
        .as_deref()
        .or(settings.model.as_deref())
        .unwrap_or(DEFAULT_MODEL);
    let model = registry
        .find(query)
        .with_context(|| format!("unknown model '{query}' (see --list-models)"))?
        .clone();
    if !registry.is_supported(&model) {
        bail!(
            "no provider implementation for '{}/{}'",
            model.provider,
            model.id
        );
    }

    let auth = storage
        .resolve(&model.provider)
        .await
        .with_context(|| format!("auth resolution failed for '{}'", model.provider))?
        .with_context(|| {
            format!(
                "no credentials for provider '{}' (use --login or an env api key)",
                model.provider
            )
        })?;
    let thinking = parse_thinking(args.thinking.as_deref().or(settings.thinking.as_deref()))?;

    // The loop and the frontend are packs selected by config, not
    // hardwired (DESIGN.md WS2/WS3).
    let loop_name = settings.loop_name.as_deref().unwrap_or("flat");
    if loop_name != "flat" {
        bail!("unknown loop '{loop_name}' in settings.json (available: flat)");
    }
    let frontend = settings.frontend.as_deref().unwrap_or("inline");
    if prompt.is_none() && frontend != "inline" {
        bail!("unknown frontend '{frontend}' in settings.json (available: inline)");
    }

    // First SIGINT cancels the run (stream + tools wind down, transcript
    // stays well-formed); a second one force-quits. In the interactive
    // frontend raw mode turns Ctrl+C into a key event, so this only fires
    // for an external signal there.
    let cancel = CancellationToken::new();
    tokio::spawn({
        let cancel = cancel.clone();
        async move {
            let _ = tokio::signal::ctrl_c().await;
            eprintln!("\nphi: interrupted (Ctrl+C again to force quit)");
            cancel.cancel();
            let _ = tokio::signal::ctrl_c().await;
            phi_tui::emergency_restore();
            std::process::exit(130);
        }
    });

    // Extension host: native tools go in through the same public registry
    // extensions use, and before discovery so an extension colliding with a
    // builtin fails as a load error on stderr, not an app abort. The login
    // and list-models paths above stay extension-free (bare core).
    let host = phi_host::Host::new()?;
    phi_agent::tools::register_all(&host, cancel.clone()).context("registering native tools")?;
    extensions::load_discovered(&host)?;
    let host = Arc::new(host);
    extensions::announce(
        &host,
        "session_start",
        serde_json::json!({
            "reason": "startup",
            "cwd": std::env::current_dir()?.display().to_string(),
        }),
    );

    match prompt {
        Some(prompt) => {
            agent::run(agent::RunSpec {
                host,
                registry: &registry,
                model,
                auth,
                thinking,
                system: args.system.clone(),
                prompt,
                verbose: args.verbose,
                cancel,
            })
            .await
        }
        None => {
            chat::run(chat::ChatSpec {
                host,
                registry,
                model,
                auth,
                thinking,
                system: args.system.clone(),
                verbose: args.verbose,
                cancel,
            })
            .await
        }
    }
}

fn parse_thinking(level: Option<&str>) -> Result<ThinkingLevel> {
    let Some(level) = level else {
        return Ok(ThinkingLevel::Off);
    };
    ThinkingLevel::ALL
        .into_iter()
        .find(|candidate| candidate.as_str() == level)
        .with_context(|| {
            format!("unknown thinking level '{level}' (off|minimal|low|medium|high|xhigh)")
        })
}

fn read_stdin() -> Result<String> {
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .context("failed to read prompt from stdin")?;
    Ok(buf)
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
