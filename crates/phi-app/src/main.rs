//! The `phi` binary — WS2.6 bare core (doctrine 06): the substrate with
//! zero packs still boots. Three entry points: `--login`,
//! `--list-models [search]`, and `phi "prompt"` streaming a raw
//! completion (no tool loop — the loop is the WS4 pack). Mode
//! selection, sessions, and the Lua frontends land with WS3+; this file
//! stays a thin dispatcher (spec: `main.ts`).

use std::io::{IsTerminal, Write};
use std::process::ExitCode;

use phi_ai::protocols::SimpleStreamOptions;
use phi_ai_types::{
    AssistantMessageEvent, Context, Message, ModelThinkingLevel, ThinkingLevel, UserMessage,
    clamp_thinking_level,
};
use phi_app::cli::args::{Args, help_text, parse_args};
use phi_app::cli::list_models::render_model_list;
use phi_app::cli::login::run_login;
use phi_app::config::VERSION;
use phi_app::core::auth_guidance::format_no_models_available_message;
use phi_app::core::auth_storage::AuthStorage;
use phi_app::core::defaults::DEFAULT_THINKING_LEVEL;
use phi_app::core::model_registry::{ModelRegistry, ResolvedRequestAuth};
use phi_app::core::model_resolver::{find_initial_model, resolve_cli_model};
use phi_app::core::settings_manager::{SettingsManager, SettingsManagerCreateOptions};

/// Minimal chalk analogue: color only when the stream is a terminal.
fn stderr_paint(code: &str, text: &str) -> String {
    if std::io::stderr().is_terminal() {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_owned()
    }
}

fn error_line(text: &str) {
    eprintln!("{}", stderr_paint("31", text));
}

fn warning_line(text: &str) {
    eprintln!("{}", stderr_paint("33", text));
}

fn main() -> ExitCode {
    let args = parse_args(std::env::args().skip(1));

    // Spec: report parse diagnostics; errors are fatal.
    let mut had_error = false;
    for diagnostic in &args.diagnostics {
        if diagnostic.is_error {
            had_error = true;
            error_line(&format!("Error: {}", diagnostic.message));
        } else {
            warning_line(&format!("Warning: {}", diagnostic.message));
        }
    }
    if had_error {
        return ExitCode::FAILURE;
    }

    if args.version {
        println!("{VERSION}");
        return ExitCode::SUCCESS;
    }

    if args.help {
        print!("{}", help_text());
        return ExitCode::SUCCESS;
    }

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            error_line(&format!("Error: {error}"));
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(run(args))
}

async fn run(args: Args) -> ExitCode {
    let mut auth_storage = AuthStorage::create(None);
    for error in auth_storage.drain_errors() {
        warning_line(&format!("Warning: {error}"));
    }

    if let Some(provider) = &args.login {
        return match run_login(&mut auth_storage, provider.as_deref()).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                error_line(&format!("Error: {error}"));
                ExitCode::FAILURE
            }
        };
    }

    let mut model_registry = ModelRegistry::new(auth_storage);

    if let Some(search) = &args.list_models {
        if let Some(error) = model_registry.get_error() {
            warning_line(&format!("Warning: errors loading models.json:\n{error}"));
        }
        println!("{}", render_model_list(&model_registry, search.as_deref()));
        return ExitCode::SUCCESS;
    }

    if args.messages.is_empty() {
        // Interactive mode is WS8; the bare core's degenerate frontend
        // has nothing to do without a prompt.
        print!("{}", help_text());
        return ExitCode::SUCCESS;
    }

    // Spec (`main.ts`): the startup settings manager is created with
    // the default trust (the trust-prompt wiring is WS7 glue).
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let mut settings_manager =
        SettingsManager::create(&cwd, None, SettingsManagerCreateOptions::default());
    for error in settings_manager.drain_errors() {
        warning_line(&format!(
            "Warning: error loading {} settings: {}",
            error.scope, error.error
        ));
    }

    // Spec (`buildSessionOptions` / `findInitialModel`): CLI model wins
    // (a `:<thinking>` suffix applies unless --thinking is explicit),
    // else the settings default, else the first available model
    // preferring provider defaults.
    let mut cli_thinking = args.thinking;
    let model = if args.model.is_some() {
        let resolved = resolve_cli_model(
            args.provider.as_deref(),
            args.model.as_deref(),
            &model_registry,
        );
        if let Some(warning) = &resolved.warning {
            warning_line(&format!("Warning: {warning}"));
        }
        if let Some(error) = &resolved.error {
            error_line(&format!("Error: {error}"));
            return ExitCode::FAILURE;
        }
        if cli_thinking.is_none() {
            cli_thinking = resolved.thinking_level;
        }
        resolved.model
    } else {
        find_initial_model(
            &model_registry,
            settings_manager.get_default_provider().as_deref(),
            settings_manager.get_default_model().as_deref(),
            settings_manager.get_default_thinking_level(),
        )
        .model
    };

    let Some(model) = model else {
        error_line(&format_no_models_available_message());
        return ExitCode::FAILURE;
    };

    // Spec: `--api-key` requires a model selected via --model.
    if let Some(api_key) = &args.api_key {
        if args.model.is_none() {
            error_line(
                "Error: --api-key requires a model to be specified via --model, --provider/--model, or --models",
            );
            return ExitCode::FAILURE;
        }
        model_registry
            .auth_storage_mut()
            .set_runtime_api_key(&model.provider, api_key);
    }

    let ResolvedRequestAuth::Ok { api_key, headers } =
        model_registry.get_api_key_and_headers(&model).await
    else {
        error_line(&format_no_models_available_message());
        return ExitCode::FAILURE;
    };

    let prompt = args.messages.join("\n\n");
    let context = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage::text(prompt))],
        tools: None,
    };

    // Spec (`sdk.ts`): CLI/suffix thinking wins, else the settings
    // default, else DEFAULT_THINKING_LEVEL; clamped to the model.
    let thinking_level = cli_thinking
        .or_else(|| settings_manager.get_default_thinking_level())
        .unwrap_or(DEFAULT_THINKING_LEVEL);
    let thinking_level = clamp_thinking_level(&model, thinking_level);

    let mut options = SimpleStreamOptions::default();
    options.base.api_key = api_key;
    options.base.headers = headers;
    options.reasoning = match thinking_level {
        ModelThinkingLevel::Off => None,
        ModelThinkingLevel::Minimal => Some(ThinkingLevel::Minimal),
        ModelThinkingLevel::Low => Some(ThinkingLevel::Low),
        ModelThinkingLevel::Medium => Some(ThinkingLevel::Medium),
        ModelThinkingLevel::High => Some(ThinkingLevel::High),
        ModelThinkingLevel::XHigh => Some(ThinkingLevel::XHigh),
    };

    let stream = match phi_ai::registry::stream_simple(&model, &context, Some(options)) {
        Ok(stream) => stream,
        Err(error) => {
            error_line(&format!("Error: {error}"));
            return ExitCode::FAILURE;
        }
    };

    let mut printed_text = false;
    while let Some(event) = stream.next().await {
        match event {
            AssistantMessageEvent::TextDelta { delta, .. } => {
                print!("{delta}");
                printed_text = true;
                let _ = std::io::stdout().flush();
            }
            AssistantMessageEvent::Done { .. } => {
                if printed_text {
                    println!();
                }
                return ExitCode::SUCCESS;
            }
            AssistantMessageEvent::Error { error, .. } => {
                if printed_text {
                    println!();
                }
                let message = error
                    .error_message
                    .unwrap_or_else(|| "Unknown error".to_owned());
                error_line(&format!("Error: {message}"));
                return ExitCode::FAILURE;
            }
            _ => {}
        }
    }

    if printed_text {
        println!();
    }
    ExitCode::SUCCESS
}
