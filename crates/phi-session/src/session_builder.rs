//! Per-project session construction shared by TUI mode and mux mode: loads
//! the project's layered config, resolves models, and assembles the engine,
//! session store, and UI handles for one interactive coding session.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use phi_ai::{AuthStorage, Model, Registry, ThinkingLevel};
use phi_builtins::bundled_extensions;
use phi_builtins::prelude::{CODING_PRELUDE, coding_tools_section};
use phi_cli::Args;
use phi_rlm::engine::PythonEnvFactory;
use phi_rlm::{ContextPayload, ProviderLm, Rlm, RlmConfig};
use phi_runtime::RuntimeBuilder;

use phi_memory::{MemoryConfig, MemoryExtension};

use crate::config::{Config, Settings};
use crate::session::SessionStore;
use crate::tui::{TuiSetup, UiSetup};

/// Process-wide handles shared by every session.
pub struct SharedResources {
    pub args: Args,
    pub storage: Arc<AuthStorage>,
    pub registry: Arc<Registry>,
}

/// Build a ready-to-run interactive session rooted at `cwd`. Each call loads
/// the project's own `.phi/config.lua`, so projects in one mux can use
/// different models, pythons, or limits. With `resume`, the session picks up
/// an existing JSONL instead of starting fresh.
pub async fn build_session(
    cwd: &Path,
    shared: &SharedResources,
    resume: Option<&Path>,
) -> Result<TuiSetup> {
    let SharedResources {
        args,
        storage,
        registry,
    } = shared;
    let settings = Settings::resolve(args, Config::load(cwd)?, true);

    let root_model = find_model(registry, &settings.model)?;
    let sub_model = match &settings.sub_model {
        Some(query) => find_model(registry, query)?,
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
    let mut missing_credentials = Vec::new();
    for provider in [&root_model.provider, &sub_model.provider] {
        if !missing_credentials.contains(provider) && storage.resolve(provider).await?.is_none() {
            missing_credentials.push(provider.clone());
        }
    }

    // Discover and load Lua extensions
    let ext_paths = phi_lua::discover_extensions(cwd);
    let lua_exts = phi_lua::load_extensions(&ext_paths);

    // Load config.lua files as LuaExtensions (phi global active — extension entry point)
    let config_ext_paths = phi_lua::config_extension_paths(cwd);
    let config_exts = phi_lua::load_extensions(&config_ext_paths);

    // Build AppRuntime
    let mut rt_builder = RuntimeBuilder::new().with_cwd(cwd);
    for ext in bundled_extensions() {
        rt_builder = rt_builder.register_boxed_extension(ext);
    }
    for ext in config_exts {
        rt_builder = rt_builder.register_boxed_extension(ext);
    }
    let memory_cfg = MemoryConfig {
        enabled: settings.memory.enabled,
        vault_path: settings.memory.vault_path.clone(),
        recall_limit: settings.memory.recall_limit,
        auto_inject: settings.memory.auto_inject,
    };
    match MemoryExtension::with_config(memory_cfg) {
        Ok(ext) => {
            rt_builder = rt_builder.register_boxed_extension(Box::new(ext));
        }
        Err(e) => eprintln!("[memory] {e}"),
    }
    for ext in lua_exts {
        rt_builder = rt_builder.register_boxed_extension(ext);
    }
    let runtime = Arc::new(rt_builder.build()?);
    runtime.boot().await?;

    // Augment setup_code with extension stubs
    let ext_stubs = runtime.extension_prelude_stubs();
    let setup_code = if ext_stubs.is_empty() {
        CODING_PRELUDE.to_owned()
    } else {
        format!("{CODING_PRELUDE}\n\n# Extension tools\n{ext_stubs}")
    };

    let (prompt_override, append_prompt) = crate::prompts::discover_prompt_overrides(cwd);

    let config = RlmConfig {
        max_depth: settings.max_depth,
        max_iterations: settings.max_iterations,
        max_budget: settings.budget,
        max_timeout: settings.timeout.map(Duration::from_secs),
        max_tokens: settings.max_tokens,
        max_errors: settings.max_errors,
        max_concurrent_subcalls: settings.max_concurrent_subcalls,
        custom_system_prompt: prompt_override,
        append_system_prompt: append_prompt,
        setup_code: Some(setup_code),
        tools_section: Some(coding_tools_section()),
        cwd: Some(cwd.display().to_string()),
        user_prologue: Some(format!(
            "You are a coding agent operating directly in the user's working directory: {}. \
             Use the pre-defined helper functions to explore and modify the project.",
            cwd.display()
        )),
        ..RlmConfig::default()
    };

    let root = Arc::new(ProviderLm::new(
        registry.clone(),
        root_model,
        storage.clone(),
        thinking,
    ));
    let sub = Arc::new(ProviderLm::new(
        registry.clone(),
        sub_model,
        storage.clone(),
        sub_model_thinking,
    ));
    let envs = Arc::new(PythonEnvFactory {
        python: settings.python.clone(),
    });

    let payload = ContextPayload::Str(workspace_context(cwd));
    let store = match resume {
        Some(path) => SessionStore::load(path)?,
        None => SessionStore::new(cwd)?,
    };
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let mut rlm = Rlm::new(config, root.clone(), sub.clone(), envs);
    register_supported_named_models(
        &mut rlm,
        registry.clone(),
        storage.clone(),
        sub_model_thinking,
    );
    let rlm = rlm.with_events(tx);
    let rlm = rlm.with_runtime(Arc::clone(&runtime));
    Ok(TuiSetup {
        rlm,
        payload,
        events: rx,
        missing_credentials,
        runtime,
        ui: UiSetup {
            max_iterations: settings.max_iterations,
            storage: storage.clone(),
            registry: registry.clone(),
            root_lm: root,
            sub_lm: sub,
            store,
        },
    })
}

pub fn find_model(registry: &Registry, query: &str) -> Result<Model> {
    registry
        .find(query)
        .cloned()
        .with_context(|| format!("unknown model '{query}' (see --list-models)"))
}

pub fn register_supported_named_models(
    rlm: &mut Rlm,
    registry: Arc<Registry>,
    storage: Arc<AuthStorage>,
    thinking: ThinkingLevel,
) {
    for model in registry.supported_models() {
        let name = format!("{}/{}", model.provider, model.id);
        let lm = Arc::new(ProviderLm::new(
            registry.clone(),
            model.clone(),
            storage.clone(),
            thinking,
        ));
        rlm.register_model(name, lm);
    }
}

pub fn workspace_context(cwd: &Path) -> String {
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
