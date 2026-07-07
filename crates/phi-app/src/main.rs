//! Bare core (DESIGN.md doctrine 06): login, list-models, and a one-shot
//! streamed completion with zero extensions loaded. Everything else arrives
//! as packs on the Lua host.

use std::io::{IsTerminal, Read as _, Write as _};
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use phi_ai::{
    AssistantEvent, AuthStorage, Context, LoginMethod, Message, OAuthProviderId, PrintCallbacks,
    Registry, StreamOptions, ThinkingLevel, UserMessage,
};
use tokio_util::sync::CancellationToken;

use crate::cli::Args;
use crate::format::{format_model_pricing, format_usage_summary};

mod cli;
mod format;

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

    let registry = Registry::builtin();

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

    let prompt = match args.prompt_text() {
        Some(text) if text == "-" => read_stdin()?,
        Some(text) => text,
        None if !std::io::stdin().is_terminal() => read_stdin()?,
        None => {
            bail!("no prompt given (the interactive frontend is not part of the bare core yet)")
        }
    };
    if prompt.trim().is_empty() {
        bail!("empty prompt");
    }

    oneshot(&args, &storage, &registry, prompt).await
}

async fn oneshot(
    args: &Args,
    storage: &AuthStorage,
    registry: &Registry,
    prompt: String,
) -> Result<()> {
    let query = args.model.as_deref().unwrap_or(DEFAULT_MODEL);
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

    let mut opts = StreamOptions::new(auth);
    opts.thinking = parse_thinking(args.thinking.as_deref())?;

    let ctx = Context {
        system_prompt: args.system.clone(),
        messages: vec![Message::User(UserMessage::new(prompt))],
        ..Context::default()
    };

    let mut stream = registry.stream(&model, &ctx, opts);
    let mut stdout = std::io::stdout();
    let mut wrote = false;
    while let Some(event) = stream.next().await {
        match event {
            AssistantEvent::TextDelta { delta, .. } => {
                stdout.write_all(delta.as_bytes())?;
                stdout.flush()?;
                wrote = true;
            }
            AssistantEvent::Done { message } => {
                if wrote && !message.text().ends_with('\n') {
                    println!();
                }
                eprintln!("{}", format_usage_summary(&message.usage));
                return Ok(());
            }
            AssistantEvent::Error { message } => {
                if wrote {
                    println!();
                }
                bail!(
                    "provider error: {}",
                    message
                        .error_message
                        .unwrap_or_else(|| "unknown error".to_owned())
                );
            }
            _ => {}
        }
    }
    bail!("stream ended without a terminal event")
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
