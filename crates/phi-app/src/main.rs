#[cfg(feature = "jemalloc")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[cfg(all(feature = "jemalloc", not(feature = "jemalloc-prof")))]
#[allow(non_upper_case_globals)]
#[unsafe(no_mangle)]
pub static malloc_conf: Option<&'static [u8; 50]> =
    Some(b"dirty_decay_ms:1000,muzzy_decay_ms:1000,narenas:4\0");

#[cfg(feature = "jemalloc-prof")]
#[allow(non_upper_case_globals)]
#[unsafe(no_mangle)]
pub static malloc_conf: Option<&'static [u8; 78]> =
    Some(b"dirty_decay_ms:1000,muzzy_decay_ms:1000,narenas:4,prof:true,prof_active:false\0");

use std::io::IsTerminal;
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use phi_ai::{
    AuthStorage, LoginMethod, OAuthProviderId, PrintCallbacks, Registry,
};
use phi_cli::{Args, Command};
use phi_mux as mux;
use phi_protocol as protocol;
use phi_server as server;
use phi_session::config;
use phi_session_tui::{session_builder, tui};
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let storage = Arc::new(AuthStorage::new(auth_path()?));

    // Dispatch subcommands (serve / connect / stop) before the legacy flat-arg
    // surface, mirroring ref/jcode's `cli::dispatch::run_main`.
    if let Some(command) = &args.command {
        match command {
            Command::Serve => return run_serve().await,
            Command::Connect => return run_connect().await,
            Command::Stop => return run_stop().await,
        }
    }

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

mod format;
mod oneshot;
mod render;
mod tool_view;

#[cfg(test)]
mod tests;

use format::format_model_pricing;

/// Dispatch to the appropriate execution mode: mux, TUI, or one-shot CLI.
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

    oneshot::run(args, storage, registry, cwd).await
}

/// Start the agent server daemon. Mirrors ref/jcode's `Command::Serve`.
async fn run_serve() -> Result<()> {
    let srv = server::Server::new();
    srv.run().await
}

/// Connect to a running server as a simple line-mode client. Mirrors
/// ref/jcode's `tui_launch::run_client`.
async fn run_connect() -> Result<()> {
    use std::io::{self, Write as _};

    let mut client = server::Client::connect().await?;

    if !client.ping().await? {
        bail!("Failed to ping server");
    }

    println!("Connected to phi server");
    println!("Type your message, or 'quit' to exit.
");

    loop {
        print!("> ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim();
        if input.is_empty() {
            continue;
        }
        if input == "quit" || input == "exit" {
            break;
        }

        match client.send_message(input).await {
            Ok(msg_id) => loop {
                match client.read_event().await {
                    Ok(event) => {
                        use protocol::ServerEvent;
                        match event {
                            ServerEvent::TextDelta { text } => {
                                print!("{text}");
                                let _ = io::stdout().flush();
                            }
                            ServerEvent::Done { id } if id == msg_id => {
                                break;
                            }
                            ServerEvent::Error { id, message, .. } if id == msg_id => {
                                eprintln!("
Error: {message}");
                                break;
                            }
                            _ => {}
                        }
                    }
                    Err(e) => {
                        eprintln!("
Event error: {e}");
                        break;
                    }
                }
            },
            Err(e) => {
                eprintln!("Error: {e}");
            }
        }

        println!();
    }

    Ok(())
}

/// Stop a running server by connecting and sending a Shutdown request.
async fn run_stop() -> Result<()> {
    if !server::Server::is_running().await {
        println!("No server running on {}", protocol::socket_path().display());
        return Ok(());
    }
    let mut client = server::Client::connect().await?;
    client.shutdown().await?;
    println!("Server shutdown requested");
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
