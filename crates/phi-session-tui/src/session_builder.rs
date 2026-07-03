//! Per-project session construction shared by TUI mode and mux mode: loads
//! the project's layered config, resolves models, and assembles the engine,
//! session store, and UI handles for one interactive coding session.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use phi_ai::{AuthStorage, Model, Registry, ThinkingLevel};
use phi_builtins::prelude::{CODING_PRELUDE, coding_tools_section};
use phi_ext::RuntimeBuilder;
use phi_rlm::{ContextPayload, ProviderLm, Rlm, RlmConfig};
use phi_session::config::CliArgs;

use crate::tui::{TuiSetup, UiSetup};
use phi_session::config::{Config, Settings};
use phi_session::session::SessionStore;

/// Process-wide handles shared by every session.
pub struct SharedResources {
    pub cli_args: CliArgs,
    /// Extra `--project` directories for the multiplexer (repeatable).
    pub projects: Vec<String>,
    pub storage: Arc<AuthStorage>,
    pub registry: Arc<Registry>,
    /// Whether an undecided project-trust question may be asked on stdin.
    /// Only the standalone TUI (which builds its session before entering raw
    /// mode) can prompt; the mux and one-shot modes fall back to
    /// untrusted-with-notice.
    pub trust_prompt: bool,
}

/// Resolved project-trust state for one session build.
pub struct ProjectTrust {
    pub trusted: bool,
    /// User-facing notice to render in the session greeting.
    pub notice: Option<String>,
}

/// Resolve whether `cwd`'s project-local `.phi/` content (extensions,
/// config.lua, skills, prompt overrides) may load. First run in a project
/// with such content prompts (when allowed) and persists the answer; until
/// trusted, nothing project-local loads.
pub fn resolve_project_trust(cwd: &Path, allow_prompt: bool) -> ProjectTrust {
    use phi_session::trust::{TrustStore, has_project_content};
    if !has_project_content(cwd) {
        return ProjectTrust {
            trusted: true,
            notice: None,
        };
    }
    let mut store = TrustStore::load_default();
    match store.decision(cwd) {
        Some(true) => ProjectTrust {
            trusted: true,
            notice: None,
        },
        Some(false) => ProjectTrust {
            trusted: false,
            notice: Some(
                "project .phi/ extensions are untrusted — /trust to enable them".to_owned(),
            ),
        },
        None if allow_prompt => {
            let trusted = prompt_project_trust(cwd);
            let _ = store.set(cwd, trusted);
            ProjectTrust {
                trusted,
                notice: (!trusted).then(|| {
                    "project .phi/ extensions not loaded — /trust to enable them".to_owned()
                }),
            }
        }
        None => ProjectTrust {
            trusted: false,
            notice: Some(
                "first run here: project .phi/ extensions not loaded — /trust to enable them"
                    .to_owned(),
            ),
        },
    }
}

/// One-line stdin prompt, asked before the TUI takes the terminal.
fn prompt_project_trust(cwd: &Path) -> bool {
    use std::io::Write as _;
    eprintln!(
        "This project ({}) contains .phi/ extensions, config, skills, or prompt overrides.",
        cwd.display()
    );
    eprint!("Loading them runs code with your full permissions. Load them? [y/N] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim(), "y" | "Y" | "yes" | "Yes")
}

/// Build a ready-to-run interactive session rooted at `cwd`. Each call loads
/// the project's own `.phi/config.lua`, so projects in one mux can use
/// different models, pythons, or limits. With `resume`, the session picks up
/// an existing JSONL instead of starting fresh.
/// Resolved models, thinking levels, and missing-credential list for a session.
struct ResolvedModels {
    root_model: Model,
    sub_model: Model,
    thinking: ThinkingLevel,
    sub_model_thinking: ThinkingLevel,
    missing_credentials: Vec<String>,
}

/// Resolve root/sub models, parse thinking levels, and detect missing provider
/// credentials from already-loaded settings.
async fn resolve_models(
    settings: &Settings,
    registry: &Registry,
    storage: &AuthStorage,
) -> Result<ResolvedModels> {
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

    let mut missing_credentials = Vec::new();
    for provider in [&root_model.provider, &sub_model.provider] {
        if !missing_credentials.contains(provider) && storage.resolve(provider).await?.is_none() {
            missing_credentials.push(provider.clone());
        }
    }

    Ok(ResolvedModels {
        root_model,
        sub_model,
        thinking,
        sub_model_thinking,
        missing_credentials,
    })
}

/// Discover and load Lua extensions and config.lua files, then build and boot
/// the AppRuntime with all extensions registered. Project-local `.phi/` Lua
/// loads only when the project is trusted. Shared with one-shot mode
/// (`phi-app`), so every mode resolves extensions the same way.
pub async fn build_runtime(
    cwd: &Path,
    registry: &Registry,
    settings: &Settings,
    project_trusted: bool,
) -> Result<Arc<phi_ext::AppRuntime>> {
    let ext_paths = phi_lua::discover_extensions(cwd, project_trusted);
    let lua_exts = phi_lua::load_extensions(&ext_paths);

    let config_ext_paths = phi_lua::config_extension_paths(cwd, project_trusted);
    let config_exts = phi_lua::load_extensions(&config_ext_paths);

    // `/model` completion covers the embedded first-party models plus the
    // bundled catalog. Providers registered by user extensions still miss
    // completion (they register during the build below) — known WS3.2 gap.
    #[cfg(feature = "builtins")]
    let model_names: Vec<String> = registry
        .supported_models()
        .map(|m| format!("{}/{}", m.provider, m.id))
        .chain(phi_builtins::catalog::model_names())
        .collect();

    let mut rt_builder = RuntimeBuilder::new().with_cwd(cwd);
    // Built-ins register through the same extension API user extensions use:
    // the bundled provider catalog, the default visual set (spinner, thinking
    // expressions, theme), and the default slash commands. The catalog goes
    // first so a user extension re-registering a bundled provider name wins
    // (provider registration is last-write-wins), e.g. to route it through a
    // proxy or attach an inline api_key. Without the `builtins` feature the
    // bare core boots with no builtin extensions registered (WS1.5).
    #[cfg(feature = "builtins")]
    {
        rt_builder = rt_builder.register_extension(phi_builtins::CatalogExtension);
        rt_builder = rt_builder.register_extension(phi_builtins::DefaultVisualExtension);
        rt_builder =
            rt_builder.register_extension(phi_builtins::BuiltinCommandsExtension::new(model_names));
        rt_builder = rt_builder.register_extension(phi_builtins::SkillsExtension::new(
            cwd,
            settings.skills.clone(),
            project_trusted,
        ));
    }
    for ext in config_exts {
        rt_builder = rt_builder.register_boxed_extension(ext);
    }
    for ext in lua_exts {
        rt_builder = rt_builder.register_boxed_extension(ext);
    }
    let runtime = Arc::new(rt_builder.build()?);
    runtime.boot().await?;
    runtime.start_session().await?;

    // ── Extension: ProjectTrust ──────────────────────────────────────
    // Let extensions gate project trust. If an extension says the project
    // is untrusted, we log a warning but don't block (extensions can
    // handle this themselves via the event).
    match runtime
        .check_project_trust(&cwd.display().to_string())
        .await
    {
        Ok(decision) if !decision.trusted => {
            eprintln!(
                "Warning: project trust check returned untrusted for {}",
                cwd.display()
            );
        }
        Ok(_) => {}
        Err(_) => {}
    }

    // ── Extension: ResourcesDiscover ─────────────────────────────────
    // Let extensions contribute additional skills, prompts, and themes.
    match runtime
        .discover_resources(&cwd.display().to_string(), "startup")
        .await
    {
        Ok(resources) => {
            // Resources are available on the runtime; the prompt builder
            // and skill discovery can query them later if needed.
            let _ = resources;
        }
        Err(_) => {}
    }

    Ok(runtime)
}

/// Build RlmConfig from resolved settings, prompt overrides, and environment info.
fn build_rlm_config(
    cwd: &Path,
    settings: &Settings,
    runtime: &phi_ext::AppRuntime,
    project_trusted: bool,
) -> RlmConfig {
    let ext_stubs = runtime.extension_prelude_stubs();
    let setup_code = if ext_stubs.is_empty() {
        CODING_PRELUDE.to_owned()
    } else {
        format!("{CODING_PRELUDE}\n\n# Extension tools\n{ext_stubs}")
    };

    let (prompt_override, append_prompt) = phi_rlm::discover_prompt_overrides(cwd, project_trusted);

    // Coding-session prompt shape (WS9.4): the coding identity replaces the
    // long-context-QA RLM identity (unless a .phi/SYSTEM.md override wins),
    // and the orchestrator addendum is off — delegation guidance lives in
    // the identity, claude-code-shaped. The old user_prologue restated the
    // identity as a user message; the system prompt now owns it.
    RlmConfig {
        max_rlm_depth: settings.max_rlm_depth,
        max_iterations: settings.max_iterations,
        max_concurrent_subcalls: settings.max_concurrent_subcalls,
        custom_system_prompt: prompt_override.or_else(|| Some(phi_rlm::CODING_IDENTITY.to_owned())),
        append_system_prompt: append_prompt,
        setup_code: Some(setup_code),
        tools_section: Some(coding_tools_section()),
        env_info: Some(env_info_block(cwd)),
        cwd: Some(cwd.display().to_string()),
        orchestrator: false,
        ..RlmConfig::default()
    }
}

/// Graft extension-registered providers (`register_provider`) onto the
/// embedded catalog: each provider's model configs become registry models,
/// and an inline `api_key` becomes a runtime-only auth override (never
/// persisted). Returns the extended registry plus user-facing notices for
/// entries that couldn't be used.
pub fn registry_with_extension_providers(
    base: &Registry,
    runtime: &phi_ext::AppRuntime,
    storage: &AuthStorage,
) -> (Arc<Registry>, Vec<String>) {
    let mut extra = Vec::new();
    let mut notices = Vec::new();
    for name in runtime.list_providers() {
        let Some(config) = runtime.get_provider(&name) else {
            continue;
        };
        if let Some(key) = &config.api_key {
            storage.set_runtime_api_key(&config.name, key);
        }
        extra.extend(extension_provider_models(config, &mut notices));
    }
    (Arc::new(base.extended(extra)), notices)
}

pub fn extension_provider_models(
    config: &phi_ext::ProviderConfig,
    notices: &mut Vec<String>,
) -> Vec<Model> {
    use phi_ai::types::{Api, CostRates, Modality};
    // The generic OpenAI-compatible protocol is the default: it's what
    // nearly every third-party endpoint speaks.
    let api_name = config.api.as_deref().unwrap_or("openai-completions");
    let api: Api = match serde_json::from_value(serde_json::Value::String(api_name.to_owned())) {
        Ok(api) => api,
        Err(_) => {
            notices.push(format!(
                "extension provider '{}': unknown api '{}' — skipped",
                config.name, api_name
            ));
            return Vec::new();
        }
    };
    let Some(base_url) = config.base_url.as_deref() else {
        notices.push(format!(
            "extension provider '{}': no base_url — skipped",
            config.name
        ));
        return Vec::new();
    };
    let base_url = base_url.trim_end_matches('/');
    config
        .models
        .iter()
        .map(|m| {
            let thinking_levels = m.thinking_levels.as_ref().and_then(|levels| {
                let parsed: std::collections::BTreeMap<ThinkingLevel, Option<String>> = levels
                    .iter()
                    .filter_map(|(level, value)| match level.parse::<ThinkingLevel>() {
                        Ok(level) => Some((level, value.clone())),
                        Err(_) => {
                            notices.push(format!(
                                "extension provider '{}' model '{}': \
                                 unknown thinking level '{level}' — ignored",
                                config.name, m.id
                            ));
                            None
                        }
                    })
                    .collect();
                (!parsed.is_empty()).then_some(parsed)
            });
            Model {
                id: m.id.clone(),
                name: if m.name.is_empty() {
                    m.id.clone()
                } else {
                    m.name.clone()
                },
                api,
                provider: config.name.clone(),
                base_url: base_url.to_owned(),
                reasoning: m.reasoning,
                input: match &m.input {
                    Some(modalities) if modalities.iter().any(|i| i == "image") => {
                        vec![Modality::Text, Modality::Image]
                    }
                    _ => vec![Modality::Text],
                },
                cost: m.cost.map_or_else(Default::default, |c| CostRates {
                    input: c.input,
                    output: c.output,
                    cache_read: c.cache_read,
                    cache_write: c.cache_write,
                }),
                // Unset limits get conservative defaults rather than zero, which
                // would trip the overflow logic immediately.
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
                headers: m.headers.clone().into_iter().collect(),
                thinking_levels,
                adaptive_thinking: m.adaptive_thinking,
            }
        })
        .collect()
}

/// Build root and sub ProviderLm handles from resolved models.
fn build_providers(
    registry: &Arc<Registry>,
    storage: &Arc<AuthStorage>,
    models: &ResolvedModels,
    max_tokens: Option<u64>,
) -> (Arc<ProviderLm>, Arc<ProviderLm>) {
    let root = Arc::new(ProviderLm::new(
        registry.clone(),
        models.root_model.clone(),
        storage.clone(),
        models.thinking,
        max_tokens,
    ));
    let sub = Arc::new(ProviderLm::new(
        registry.clone(),
        models.sub_model.clone(),
        storage.clone(),
        models.sub_model_thinking,
        max_tokens,
    ));
    (root, sub)
}

pub async fn build_session(
    cwd: &Path,
    shared: &SharedResources,
    resume: Option<&Path>,
) -> Result<TuiSetup> {
    let SharedResources {
        cli_args,
        projects: _,
        storage,
        registry,
        trust_prompt,
    } = shared;
    let settings = Settings::resolve(cli_args, Config::load(cwd)?, true);

    // Trust gates everything project-local below (extensions, config.lua,
    // skills, prompt overrides); resolve it first, prompting only when the
    // terminal is still ours.
    let trust = resolve_project_trust(cwd, *trust_prompt);

    // The runtime boots before model resolution so extension-registered
    // providers (`register_provider`) can contribute models to the registry.
    let runtime = build_runtime(cwd, registry, &settings, trust.trusted).await?;
    let (registry, provider_notices) =
        registry_with_extension_providers(registry, &runtime, storage);
    let models = resolve_models(&settings, &registry, storage).await?;
    let config = build_rlm_config(cwd, &settings, &runtime, trust.trusted);
    let (root, sub) = build_providers(&registry, storage, &models, settings.max_tokens);

    // Wire the extension runtime into the providers so provider
    // request/response interception and message streaming events fire.
    root.set_runtime(Arc::clone(&runtime));
    sub.set_runtime(Arc::clone(&runtime));

    let payload = ContextPayload::Str(workspace_context(cwd));
    let store = match resume {
        Some(path) => {
            let mut store = SessionStore::load(path)?;
            // Opening for use is the one safe moment to reclaim dead log
            // lines (stale leafs, closed exchange markers). Best-effort:
            // a failed compact must not fail the resume.
            let _ = store.compact();
            store
        }
        None => SessionStore::new(cwd)?,
    };
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let mut rlm = Rlm::new(config, root.clone(), sub.clone(), settings.python.clone());
    register_supported_named_models(
        &mut rlm,
        registry.clone(),
        storage.clone(),
        models.sub_model_thinking,
    );
    let rlm = rlm.with_events(tx);
    let rlm = rlm.with_runtime(Arc::clone(&runtime));
    Ok(TuiSetup {
        rlm,
        payload,
        events: rx,
        missing_credentials: models.missing_credentials,
        notices: trust.notice.into_iter().chain(provider_notices).collect(),
        runtime,
        ui: UiSetup {
            max_iterations: settings.max_iterations,
            tools: settings.tools.clone(),
            storage: storage.clone(),
            registry: registry.clone(),
            root_lm: root,
            sub_lm: sub,
            store,
        },
    })
}

pub fn find_model(registry: &Registry, query: &str) -> Result<Model> {
    registry.find(query).cloned().with_context(|| {
        match query.split_once('/') {
            // An unknown provider is a different failure than an unknown
            // model: nothing bundled or extension-registered claims the name.
            Some((provider, _)) if registry.first_for_provider(provider).is_none() => format!(
                "unknown model '{query}': no provider '{provider}' is bundled or \
                 registered (an extension can add one via phi.register_provider; \
                 see --list-models)"
            ),
            _ => format!("unknown model '{query}' (see --list-models)"),
        }
    })
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
            None,
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

/// Compute the `# Environment` system-prompt block once per session (cwd,
/// is-git, platform, shell, OS version), mirroring claude-code's
/// `computeSimpleEnvInfo`. Provider-specific lines (model marketing name,
/// knowledge cutoff, fast mode) are dropped — Phi is multi-provider. Memoized
/// by construction: the caller stores the result in `RlmConfig::env_info` and
/// the prompt is rebuilt deterministically each turn from it, so this is not
/// recomputed per turn (only on session build / cwd change).
pub fn env_info_block(cwd: &Path) -> String {
    let is_git = is_git_repo(cwd);
    let platform = std::env::consts::OS;
    let shell = std::env::var("SHELL")
        .or_else(|_| std::env::var("COMSPEC"))
        .unwrap_or_else(|_| "unknown".to_owned());
    let os_version = os_version();
    format!(
        "# Environment\n\
         You have been invoked in the following environment: \n\
         - Primary working directory: {cwd}\n\
         - Is a git repository: {is_git}\n\
         - Platform: {platform}\n\
         - Shell: {shell}\n\
         - OS Version: {os_version}",
        cwd = cwd.display(),
        is_git = if is_git { "Yes" } else { "No" },
        platform = platform,
        shell = shell,
        os_version = os_version,
    )
}

/// Walk up from `start` looking for a `.git` entry (directory or worktree
/// `gitdir:` file), mirroring `getIsGit` / `git_branch` in `tui.rs`.
fn is_git_repo(start: &Path) -> bool {
    let mut dir = Some(start);
    while let Some(d) = dir {
        if d.join(".git").exists() {
            return true;
        }
        dir = d.parent();
    }
    false
}

/// `uname -sr` equivalent: "Linux 7.0.11", "Darwin 25.3.0". Reads
/// `/proc/sys/kernel/osrelease` on Linux (no spawn); falls back to `uname -sr`
/// then to a generic constant.
fn os_version() -> String {
    if cfg!(target_os = "linux")
        && let Ok(release) = std::fs::read_to_string("/proc/sys/kernel/osrelease")
    {
        let release = release.trim();
        if !release.is_empty() {
            return format!("Linux {release}");
        }
    }
    if cfg!(any(target_os = "linux", target_os = "macos"))
        && let Ok(out) = std::process::Command::new("uname").args(["-sr"]).output()
        && out.status.success()
    {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if !s.is_empty() {
            return s;
        }
    }
    format!("{} unknown", std::env::consts::OS)
}

#[cfg(test)]
mod env_info_tests {
    use super::{env_info_block, is_git_repo, os_version};
    use std::path::Path;

    #[test]
    fn env_info_block_lists_cwd_git_platform_shell_os() {
        let block = env_info_block(Path::new("/home/y0usaf/Dev/phi"));
        assert!(block.starts_with("# Environment\n"));
        assert!(block.contains("Primary working directory: /home/y0usaf/Dev/phi"));
        assert!(block.contains("Is a git repository:"));
        assert!(block.contains("Platform:"));
        assert!(block.contains("Shell:"));
        assert!(block.contains("OS Version:"));
        // No provider-specific lines are ported.
        assert!(!block.contains("knowledge cutoff"));
        assert!(!block.contains("Fast mode"));
    }

    #[test]
    fn is_git_repo_detects_repository_root() {
        // Use a temp dir with a .git marker so the test does not depend on
        // the build environment being inside a real git repo (Nix sandboxes
        // copy sources to /build/source without .git).
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        assert!(is_git_repo(dir.path()));
        // A subdir should also detect the parent's .git.
        let sub = dir.path().join("nested");
        std::fs::create_dir(&sub).unwrap();
        assert!(is_git_repo(&sub));
        // A non-existent path is not a git repo.
        assert!(!is_git_repo(Path::new(
            "/nonexistent/path/that/does/not/exist"
        )));
    }

    #[test]
    fn os_version_is_non_empty() {
        let v = os_version();
        assert!(!v.trim().is_empty());
    }
}

/// Track 3: kill tool-name drift between the coding prelude and the model's
/// reachable tool surface. The handwritten `coding_tools_section()` roster is
/// the only thing tying the model's `read_file`/`edit_file`/... references to
/// real REPL functions. This test parses the real roster out of
/// `coding_tools_section()` and asserts every documented helper is present.
#[cfg(test)]
mod prompt_drift_tests {
    use std::collections::HashSet;

    use phi_builtins::prelude::coding_tools_section;

    /// Parse the helper function names out of `coding_tools_section()`, whose
    /// roster lines look like `- bash(cmd: str, ...) -> str — ...`.
    fn coding_roster(section: &str) -> HashSet<String> {
        let mut set = HashSet::new();
        for line in section.lines() {
            let line = line.trim_start();
            if let Some(rest) = line.strip_prefix("- ")
                && let Some(paren) = rest.find('(')
            {
                let name = rest[..paren].trim();
                if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    set.insert(name.to_owned());
                }
            }
        }
        set
    }

    #[test]
    fn coding_roster_lists_expected_file_helpers() {
        let roster = coding_roster(&coding_tools_section());
        for expected in [
            "bash",
            "read_file",
            "write_file",
            "edit_file",
            "grep",
            "find",
            "ls",
        ] {
            assert!(
                roster.contains(expected),
                "coding_tools_section() no longer documents `{expected}` — prompt/prelude drift"
            );
        }
    }
}

#[cfg(test)]
mod extension_provider_tests {
    use phi_ai::types::{Api, Modality};
    use phi_ext::{ProviderConfig, ProviderModelConfig};

    use super::extension_provider_models;

    fn config(api: Option<&str>, base_url: Option<&str>) -> ProviderConfig {
        ProviderConfig {
            name: "my-router".to_owned(),
            base_url: base_url.map(str::to_owned),
            api_key: Some("sk-test".to_owned()),
            api: api.map(str::to_owned),
            models: vec![ProviderModelConfig {
                id: "my-model".to_owned(),
                name: "My Model".to_owned(),
                reasoning: true,
                context_window: 0,
                max_tokens: 0,
                cost: None,
                thinking_levels: None,
                input: None,
                headers: Default::default(),
                adaptive_thinking: false,
            }],
        }
    }

    #[test]
    fn converts_models_defaulting_api_and_limits() {
        let mut notices = Vec::new();
        let models =
            extension_provider_models(&config(None, Some("https://r.example/v1/")), &mut notices);
        assert!(notices.is_empty());
        assert_eq!(models.len(), 1);
        let m = &models[0];
        assert_eq!(m.api, Api::OpenAiCompletions);
        assert_eq!(m.provider, "my-router");
        assert_eq!(m.base_url, "https://r.example/v1");
        assert!(m.reasoning);
        assert_eq!(m.input, vec![Modality::Text]);
        // Unset limits become conservative defaults, not zero.
        assert_eq!(m.context_window, 128_000);
        assert_eq!(m.max_tokens, 8_192);
    }

    #[test]
    fn explicit_api_is_honored() {
        let mut notices = Vec::new();
        let models = extension_provider_models(
            &config(Some("anthropic-messages"), Some("https://r.example")),
            &mut notices,
        );
        assert_eq!(models[0].api, Api::AnthropicMessages);
    }

    #[test]
    fn full_fidelity_fields_survive_the_graft() {
        use phi_ext::ProviderModelCost;

        let mut cfg = config(None, Some("https://r.example"));
        let m = &mut cfg.models[0];
        m.cost = Some(ProviderModelCost {
            input: 1.5,
            output: 4.5,
            cache_read: 0.4,
            cache_write: 0.0,
        });
        m.thinking_levels = Some(
            [
                ("off".to_owned(), Some("none".to_owned())),
                ("high".to_owned(), Some("high".to_owned())),
                ("bogus".to_owned(), None),
            ]
            .into(),
        );
        m.input = Some(vec!["text".to_owned(), "image".to_owned()]);

        let mut notices = Vec::new();
        let models = extension_provider_models(&cfg, &mut notices);
        let m = &models[0];
        assert_eq!(m.cost.input, 1.5);
        assert_eq!(m.cost.output, 4.5);
        assert_eq!(m.input, vec![Modality::Text, Modality::Image]);
        let levels = m.thinking_levels.as_ref().unwrap();
        assert_eq!(levels.len(), 2, "bogus level dropped, valid ones kept");
        // The dropped level is reported, not silently eaten.
        assert_eq!(notices.len(), 1);
        assert!(notices[0].contains("bogus"));
    }

    #[test]
    fn unknown_api_or_missing_base_url_skip_with_notice() {
        let mut notices = Vec::new();
        assert!(
            extension_provider_models(&config(Some("nope"), Some("https://x")), &mut notices)
                .is_empty()
        );
        assert!(extension_provider_models(&config(None, None), &mut notices).is_empty());
        assert_eq!(notices.len(), 2);
        assert!(notices[0].contains("unknown api"));
        assert!(notices[1].contains("no base_url"));
    }
}
