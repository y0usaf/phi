//! Extension host wiring for the bare core: discover, gate project-local
//! Lua on trust, load, and announce the session. The trust *prompt* lives
//! here — phi-host is headless and only remembers decisions.

use std::io::{IsTerminal as _, Write as _};
use std::path::Path;

use anyhow::{Context as _, Result};
use phi_ai::{Api, AuthStorage, Modality, Model, ReasoningControl, Registry};
use phi_host::{Host, TrustDecision, TrustStore};

/// The shipped built-ins: the first-party provider catalog and the default
/// slash commands, both embedded Lua registered through the same public API
/// user extensions use (`phi.register_provider`, `phi.register_command`) —
/// The Rule made literal. Provider re-registration replaces, so a later
/// user extension can override any first-party provider; a command name
/// collision with a builtin is a load error for the extension.
pub fn load_builtins(host: &Host) -> Result<()> {
    host.load_source(
        "<builtin:providers>",
        include_str!("builtins/providers.lua"),
    )?;
    host.load_source("<builtin:commands>", include_str!("builtins/commands.lua"))
}

/// Discover and load extensions onto an existing host. With no extension
/// files present this is the zero-extension bare boot (doctrine 06) — the
/// host exists but nothing extension-shaped is registered. A failing
/// extension (including a name collision with an earlier registration) is
/// reported on stderr and skipped, never fatal.
pub fn load_discovered(host: &Host) -> Result<()> {
    let cwd = std::env::current_dir().context("resolving cwd")?;
    let include_project = project_trusted(&cwd);
    let global_dir = phi_host::global_extensions_dir();
    for path in phi_host::discover_extensions(global_dir.as_deref(), &cwd, include_project) {
        if let Err(e) = host.load_file(&path) {
            eprintln!("phi: extension error: {e:#}");
        }
    }
    Ok(())
}

/// Emit an event and surface handler failures on stderr; handler results
/// are ignored here (the bare core has no consumer for them yet).
pub fn announce(host: &Host, event: &str, payload: serde_json::Value) {
    match host.emit(event, &payload) {
        Ok(outcomes) => {
            for outcome in outcomes {
                if let Err(e) = outcome.result {
                    eprintln!("phi: {} handler failed in {}: {e}", event, outcome.source);
                }
            }
        }
        Err(e) => eprintln!("phi: dispatching {event}: {e:#}"),
    }
}

/// Decide whether `.phi/extensions` may load: remembered decisions win;
/// otherwise prompt when interactive, skip when not.
fn project_trusted(cwd: &Path) -> bool {
    if !phi_host::project_extensions_present(cwd) {
        return false;
    }
    let Some(store_path) = TrustStore::default_path() else {
        return false;
    };
    let mut store = TrustStore::open(store_path);
    match store.decision(cwd) {
        TrustDecision::Trusted => true,
        TrustDecision::Denied => false,
        TrustDecision::Undecided => prompt_trust(&mut store, cwd),
    }
}

fn prompt_trust(store: &mut TrustStore, cwd: &Path) -> bool {
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        eprintln!(
            "phi: {} has extensions but the project is untrusted; skipping (run interactively to decide)",
            phi_host::project_extensions_dir(cwd).display()
        );
        return false;
    }
    eprint!(
        "Load project extensions from {}? They run Lua with your full permissions.\n[y]es always / [o]nce / [N]o / n[e]ver: ",
        phi_host::project_extensions_dir(cwd).display()
    );
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    match line.trim().to_lowercase().as_str() {
        "y" | "yes" => {
            if let Err(e) = store.set(cwd, true) {
                eprintln!("phi: could not persist trust decision: {e:#}");
            }
            true
        }
        "o" | "once" => true,
        "e" | "never" => {
            if let Err(e) = store.set(cwd, false) {
                eprintln!("phi: could not persist trust decision: {e:#}");
            }
            false
        }
        _ => false,
    }
}

/// Graft extension-registered providers (`phi.register_provider`) onto the
/// embedded catalog: each provider's model configs become registry `Model`s
/// (via `Registry::extended`), and an inline `api_key` becomes a runtime-only
/// auth override (never persisted to `auth.json`). Providers with no `base_url`
/// are skipped with a stderr notice — they can't resolve a request endpoint.
pub fn graft_providers(base: &Registry, host: &Host, storage: &AuthStorage) -> Registry {
    let providers = host.providers();
    if providers.is_empty() {
        return base.extended(Vec::new());
    }
    let mut extra: Vec<Model> = Vec::new();
    for p in &providers {
        // Inject inline api_key as a runtime override (never persisted).
        if let Some(key) = &p.api_key {
            storage.set_runtime_api_key(&p.name, key);
        }
        let Some(base_url) = p.base_url.as_deref() else {
            eprintln!(
                "phi: extension provider '{}': no base_url — skipped",
                p.name
            );
            continue;
        };
        let base_url = base_url.trim_end_matches('/').to_owned();
        // The generic OpenAI-compatible protocol is the default: it's what
        // nearly every third-party endpoint speaks.
        let api_name = p.api.as_deref().unwrap_or("openai-completions");
        let api: Api = match serde_json::from_value(serde_json::Value::String(api_name.to_owned()))
        {
            Ok(api) => api,
            Err(_) => {
                eprintln!(
                    "phi: extension provider '{}': unknown api '{}' — skipped",
                    p.name, api_name
                );
                continue;
            }
        };
        for m in &p.models {
            // Input modalities: declared strings typed via serde; unknown
            // strings are dropped; empty/undeclared = text-only.
            let mut input: Vec<Modality> = m
                .input
                .iter()
                .filter_map(|s| serde_json::from_value(serde_json::Value::String(s.clone())).ok())
                .collect();
            if input.is_empty() {
                input = vec![Modality::Text];
            }
            extra.push(Model {
                id: m.id.clone(),
                name: m.name.clone(),
                api,
                provider: p.name.clone(),
                base_url: base_url.clone(),
                reasoning: m.reasoning,
                input,
                cost: m
                    .cost
                    .as_ref()
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default(),
                // Unset limits get conservative defaults rather than zero,
                // which would trip the overflow logic immediately.
                context_window: if m.context_window == 0 {
                    128_000
                } else {
                    m.context_window
                },
                max_tokens: if m.max_tokens == 0 {
                    8_192
                } else {
                    m.max_tokens
                },
                headers: m.headers.clone(),
                thinking_levels: m
                    .thinking_levels
                    .as_ref()
                    .and_then(|v| serde_json::from_value(v.clone()).ok()),
                adaptive_thinking: m.adaptive_thinking,
                reasoning_control: m
                    .reasoning_control
                    .as_ref()
                    .and_then(|v| serde_json::from_value::<ReasoningControl>(v.clone()).ok()),
            });
        }
    }
    base.extended(extra)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const PROVIDER_LUA: &str = r#"
phi.register_provider("my-router", {
  base_url = "https://api.example.com/v1",
  api_key = "sk-test-inline",
  api = "openai-completions",
  models = {
    { id = "custom-gpt", name = "Custom GPT",
      reasoning = true, context_window = 128000, max_tokens = 8192 },
  }
})

phi.register_provider("vercel-proxy", {
  base_url = "https://ai-gateway.vercel.sh/v1",
  api = "anthropic-messages",
  api_key = os.getenv("AI_GATEWAY_API_KEY"),
  models = {
    { id = "claude-sonnet-4", name = "Claude Sonnet 4 (Vercel)",
      reasoning = true, context_window = 200000, max_tokens = 16384 },
  }
})
"#;

    fn temp_storage() -> (Arc<AuthStorage>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (
            Arc::new(AuthStorage::new(dir.path().join("auth.json"))),
            dir,
        )
    }

    #[test]
    fn grafted_models_are_findable_and_supported() {
        let host = Host::new().expect("host");
        host.load_source("<test:provider>", PROVIDER_LUA)
            .expect("load provider");
        let (storage, _dir) = temp_storage();
        let registry = graft_providers(&Registry::builtin(), &host, &storage);

        let builtin = Registry::builtin();
        assert!(registry.models().len() > builtin.models().len());

        let model = registry
            .find("my-router/custom-gpt")
            .expect("grafted model");
        assert_eq!(model.provider, "my-router");
        assert_eq!(model.id, "custom-gpt");
        assert_eq!(model.base_url, "https://api.example.com/v1");
        assert!(model.reasoning);
        assert_eq!(model.context_window, 128000);
        assert!(registry.is_supported(model));

        let vercel = registry
            .find("vercel-proxy/claude-sonnet-4")
            .expect("vercel model");
        assert_eq!(vercel.base_url, "https://ai-gateway.vercel.sh/v1");
        assert!(registry.is_supported(vercel));
    }

    #[test]
    fn extension_reasoning_control_threads_onto_grafted_model() {
        // The API-growth exerciser for `reasoning_control`: an extension
        // declares the wire field its model honors, and the grafted
        // `Model` carries the same typed declaration as the embedded
        // catalog (doctrine 05: one mechanism, two declaration homes).
        const LUA: &str = r#"
phi.register_provider("rc-router", {
  base_url = "https://api.example.com/v1",
  api = "openai-completions",
  api_key = "sk-test",
  models = {
    { id = "reasoning-model", name = "Reasoning Model",
      reasoning = true, context_window = 128000, max_tokens = 8192,
      reasoning_control = {
        field = "chat_template_kwargs.enable_thinking",
        values = { off = false, high = true },
      } },
  }
})
"#;
        let host = Host::new().expect("host");
        host.load_source("<test:rc>", LUA).expect("load");
        let (storage, _dir) = temp_storage();
        let registry = graft_providers(&Registry::builtin(), &host, &storage);

        let model = registry
            .find("rc-router/reasoning-model")
            .expect("grafted reasoning model");
        assert!(model.reasoning);
        let rc = model
            .reasoning_control
            .as_ref()
            .expect("reasoning_control threaded through");
        assert_eq!(rc.field, "chat_template_kwargs.enable_thinking");
        assert_eq!(
            rc.values.get(&phi_ai::ThinkingLevel::Off),
            Some(&serde_json::json!(false))
        );
        assert_eq!(
            rc.values.get(&phi_ai::ThinkingLevel::High),
            Some(&serde_json::json!(true))
        );
    }

    #[test]
    fn extension_reasoning_control_rejects_unknown_level() {
        // A typo in `values` keys is a load error, not a silently-dropped
        // level — the request builder would otherwise never see it.
        const LUA: &str = r#"
phi.register_provider("bad-rc", {
  base_url = "https://api.example.com/v1",
  api = "openai-completions",
  models = {
    { id = "x", name = "X", reasoning = true, context_window = 0, max_tokens = 0,
      reasoning_control = {
        field = "chat_template_kwargs.enable_thinking",
        values = { ofc = false },
      } },
  }
})
"#;
        let host = Host::new().expect("host");
        let err = host.load_source("<test:bad-rc>", LUA).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("unknown thinking level"),
            "expected unknown-level error, got: {msg}"
        );
    }

    #[test]
    fn inline_api_key_injected_as_runtime_override() {
        let host = Host::new().expect("host");
        host.load_source("<test:provider>", PROVIDER_LUA)
            .expect("load provider");
        let (storage, _dir) = temp_storage();
        let _registry = graft_providers(&Registry::builtin(), &host, &storage);

        let rt = tokio::runtime::Runtime::new().unwrap();
        let auth = rt
            .block_on(storage.resolve("my-router"))
            .expect("resolve")
            .expect("some auth");
        match auth {
            phi_ai::Auth::ApiKey(key) => assert_eq!(key, "sk-test-inline"),
            other => panic!("expected ApiKey, got {other:?}"),
        }

        // vercel-proxy's api_key came from os.getenv("AI_GATEWAY_API_KEY").
        // If the env var is set, the runtime override resolves to it; if not,
        // resolve falls through to env_auth (also checks AI_GATEWAY_API_KEY).
        // Either way, auth is Some(ApiKey) when the env var is present.
        let auth = rt
            .block_on(storage.resolve("vercel-proxy"))
            .expect("resolve");
        match auth {
            Some(phi_ai::Auth::ApiKey(_)) => { /* env var was set */ }
            None => { /* env var unset — both paths agree on None */ }
            other => panic!("expected ApiKey or None, got {other:?}"),
        }
    }

    #[test]
    fn provider_with_no_base_url_is_skipped() {
        let host = Host::new().expect("host");
        host.load_source(
            "<test:no-base>",
            r#"phi.register_provider("no-url", { models = {
              { id = "x", name = "X", reasoning = false, context_window = 0, max_tokens = 0 }
            } })"#,
        )
        .expect("load");
        let (storage, _dir) = temp_storage();
        let registry = graft_providers(&Registry::builtin(), &host, &storage);
        assert!(
            registry.find("no-url/x").is_none(),
            "skipped provider has no model"
        );
    }

    #[test]
    fn unknown_api_is_skipped() {
        let host = Host::new().expect("host");
        host.load_source(
            "<test:bad-api>",
            r#"phi.register_provider("bad-api", {
              base_url = "https://x.example/v1", api = "nope-protocol",
              models = { { id = "x", name = "X", reasoning = false,
                           context_window = 0, max_tokens = 0 } }
            })"#,
        )
        .expect("load");
        let (storage, _dir) = temp_storage();
        let registry = graft_providers(&Registry::builtin(), &host, &storage);
        assert!(
            registry.find("bad-api/x").is_none(),
            "unknown api skips the model"
        );
    }

    #[test]
    fn empty_providers_returns_untouched_builtin() {
        let host = Host::new().expect("host");
        let (storage, _dir) = temp_storage();
        let registry = graft_providers(&Registry::builtin(), &host, &storage);
        // No extensions loaded => the bare registry: protocols, no models.
        assert_eq!(registry.models().len(), Registry::builtin().models().len());
        assert!(registry.models().is_empty());
    }

    #[test]
    fn builtin_catalog_grafts_the_first_party_providers() {
        // The first-party catalog (builtins/providers.lua) arrives through
        // the same door user extensions use — this is the conformance test
        // that the shipped registry is fully extension-registered.
        let host = Host::new().expect("host");
        super::load_builtins(&host).expect("builtins");
        let (storage, _dir) = temp_storage();
        let registry = graft_providers(&Registry::builtin(), &host, &storage);

        // Every first-party provider is present and only via the graft.
        for provider in ["anthropic", "openai", "cursor", "neuralwatt"] {
            assert!(
                registry.first_for_provider(provider).is_some(),
                "{provider} missing from grafted catalog"
            );
        }
        assert!(registry.models().iter().all(|m| matches!(
            m.provider.as_str(),
            "anthropic" | "openai" | "cursor" | "neuralwatt"
        )));
        assert!(registry.models().iter().all(|m| registry.is_supported(m)));

        // Typed fields survive the Lua → raw JSON → Model round trip.
        let opus = registry
            .find("anthropic/claude-opus-4-7")
            .expect("default model in catalog");
        assert_eq!(opus.api, Api::AnthropicMessages);
        assert!(opus.reasoning && opus.adaptive_thinking);
        assert!(opus.cost.input > 0.0 && opus.cost.output > 0.0);
        assert!(opus.input.contains(&Modality::Image));
        let levels = opus.thinking_levels.as_ref().expect("thinking levels");
        assert_eq!(
            levels.get(&phi_ai::ThinkingLevel::High),
            Some(&Some("high".to_owned()))
        );

        let sonnet37 = registry
            .find("anthropic/claude-3-7-sonnet-20250219")
            .expect("budget model");
        assert!(!sonnet37.adaptive_thinking);
        assert_eq!(
            sonnet37
                .thinking_levels
                .as_ref()
                .and_then(|l| l.get(&phi_ai::ThinkingLevel::Low).cloned()),
            Some(Some("4096".to_owned()))
        );

        let gpt = registry.find("openai/gpt-5.1").expect("openai model");
        assert_eq!(gpt.api, Api::OpenAiResponses);

        let composer = registry.find("cursor/composer-2.5").expect("cursor");
        assert_eq!(composer.api, Api::CursorNative);
        assert_eq!(composer.base_url, "https://api2.cursor.sh");

        let glm52 = registry.find("neuralwatt/glm-5.2").expect("neuralwatt");
        assert_eq!(glm52.api, Api::OpenAiCompletions);
        assert_eq!(glm52.base_url, "https://api.neuralwatt.com/v1");
        let rc = glm52.reasoning_control.as_ref().expect("rc declared");
        assert_eq!(rc.field, "chat_template_kwargs.enable_thinking");

        // Bare ids containing slashes still resolve when unambiguous.
        let bare = registry.find("moonshotai/Kimi-K2.6").expect("bare kimi");
        assert_eq!(bare.provider, "neuralwatt");
    }

    #[test]
    fn user_extension_overrides_builtin_provider() {
        // Re-registration replaces: a user extension can re-route a
        // first-party provider (e.g. through a proxy) because builtins use
        // the same registration door and load first.
        let host = Host::new().expect("host");
        super::load_builtins(&host).expect("builtins");
        host.load_source(
            "<test:override>",
            r#"phi.register_provider("anthropic", {
              base_url = "https://my-proxy.example.com",
              api = "anthropic-messages",
              models = { { id = "claude-opus-4-7", name = "Opus via proxy",
                           reasoning = true, context_window = 200000,
                           max_tokens = 32000 } },
            })"#,
        )
        .expect("override");
        let (storage, _dir) = temp_storage();
        let registry = graft_providers(&Registry::builtin(), &host, &storage);
        let opus = registry.find("anthropic/claude-opus-4-7").expect("opus");
        assert_eq!(opus.base_url, "https://my-proxy.example.com");
        // The override replaced the whole provider entry, models included.
        assert!(registry.find("anthropic/claude-sonnet-4-5").is_none());
    }
}
