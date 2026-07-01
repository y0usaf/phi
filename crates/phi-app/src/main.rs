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

use crate::cli::Args;
use anyhow::{Context as _, Result, bail};
use phi_ai::{AuthStorage, LoginMethod, OAuthProviderId, PrintCallbacks, Registry};
use phi_mux as mux;
use phi_session::config;
use phi_session_tui::{session_builder, tui};
use tokio_util::sync::CancellationToken;

/// glibc's default arena policy (up to 8 x cores) trades memory for lock
/// contention headroom — a poor fit for a long-running TUI, where it shows
/// up as steadily growing RSS. Cap arenas before any thread allocates.
/// jemalloc builds replace the allocator wholesale via the attributes
/// above; musl has no `mallopt`, hence the `gnu` guard.
#[cfg(all(target_os = "linux", target_env = "gnu", not(feature = "jemalloc")))]
fn configure_system_allocator() {
    unsafe extern "C" {
        fn mallopt(param: i32, value: i32) -> i32;
    }

    const M_ARENA_MAX: i32 = -8;
    let arena_max = std::env::var("PHI_GLIBC_ARENA_MAX")
        .ok()
        .and_then(|value| value.trim().parse::<i32>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(4);

    let _ = unsafe { mallopt(M_ARENA_MAX, arena_max) };
}

#[cfg(not(all(target_os = "linux", target_env = "gnu", not(feature = "jemalloc"))))]
fn configure_system_allocator() {}

fn main() -> Result<()> {
    // Must run before the runtime spawns worker threads: arena caps only
    // apply to arenas that don't exist yet.
    configure_system_allocator();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to build tokio runtime")?
        .block_on(async_main())
}

async fn async_main() -> Result<()> {
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

mod cli;
mod event_print;
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
                cli_args: args.to_session_args(),
                projects: args.project.clone(),
                storage,
                registry,
            };
            return mux::app::run(shared, mux_config, cwd).await;
        }
    }

    if tui_mode {
        let shared = session_builder::SharedResources {
            cli_args: args.to_session_args(),
            projects: args.project.clone(),
            storage,
            registry,
        };
        let setup = session_builder::build_session(&cwd, &shared, None).await?;
        return tui::run(setup, tui::SessionIo::standalone(cwd)).await;
    }

    oneshot::run(args, storage, registry, cwd).await
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
