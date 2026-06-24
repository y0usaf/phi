//! Per-project session construction shared by TUI mode and mux mode: loads
//! the project's layered config, resolves models, and assembles the engine,
//! session store, and UI handles for one interactive coding session.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use phi_ai::{AuthStorage, Model, Registry, ThinkingLevel};
use phi_builtins::bundled_extensions;
use phi_builtins::prelude::{CODING_PRELUDE, coding_tools_section};
use phi_cli::Args;
use phi_rlm::{ContextPayload, ProviderLm, Rlm, RlmConfig};
use phi_runtime::RuntimeBuilder;

use phi_memory::{MemoryConfig, MemoryExtension};

use phi_session::config::{Config, Settings};
use phi_session::session::SessionStore;
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
    let settings = Settings::resolve(&args.to_session_args(), Config::load(cwd)?, true);

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

    let (prompt_override, append_prompt) = phi_session::prompts::discover_prompt_overrides(cwd);

    let env_info = env_info_block(cwd);
    let config = RlmConfig {
        max_depth: settings.max_depth,
        max_iterations: settings.max_iterations,
        max_concurrent_subcalls: settings.max_concurrent_subcalls,
        custom_system_prompt: prompt_override,
        append_system_prompt: append_prompt,
        setup_code: Some(setup_code),
        tools_section: Some(coding_tools_section()),
        env_info: Some(env_info),
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
        settings.max_tokens,
    ));
    let sub = Arc::new(ProviderLm::new(
        registry.clone(),
        sub_model,
        storage.clone(),
        sub_model_thinking,
        settings.max_tokens,
    ));

    let payload = ContextPayload::Str(workspace_context(cwd));
    let store = match resume {
        Some(path) => SessionStore::load(path)?,
        None => SessionStore::new(cwd)?,
    };
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let mut rlm = Rlm::new(config, root.clone(), sub.clone(), settings.python.clone());
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
        // This repo is a git repo (the crate root's parent chain has .git).
        assert!(is_git_repo(Path::new(env!("CARGO_MANIFEST_DIR"))));
        // A non-existent path is not a git repo.
        assert!(!is_git_repo(Path::new("/nonexistent/path/that/does/not/exist")));
    }

    #[test]
    fn os_version_is_non_empty() {
        let v = os_version();
        assert!(!v.trim().is_empty());
    }
}

/// Track 3 (plans/prompt-content-port.md): kill tool-name drift between the
/// ported prompt sections and the actual coding prelude. The prompt's hand-
/// written helper recommendations (`coding_tools_section()`-derived roster)
/// are the only thing tying the model's `read_file`/`edit_file`/... references
/// to real REPL functions. This test parses the real roster out of
/// `coding_tools_section()` and asserts every helper the ported sections name
/// in backticks exists there (or is an allowlisted core REPL builtin), and
/// that the dedicated file helpers are still both documented and recommended.
#[cfg(test)]
mod prompt_drift_tests {
    use std::collections::HashSet;

    use phi_builtins::prelude::coding_tools_section;
    use phi_rlm::prompts::{DEFAULT_ACTIONS, DEFAULT_DOING_TASKS, DEFAULT_TONE_AND_STYLE,
                           DEFAULT_USING_YOUR_TOOLS};

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
                if !name.is_empty()
                    && name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_')
                {
                    set.insert(name.to_owned());
                }
            }
        }
        set
    }

    /// Backticked identifiers in a prompt section (e.g. `` `read_file` `` or
    /// `` `bash()` ``), skipping triple-backtick ```repl``` fences.
    fn backticked_helpers(text: &str) -> Vec<String> {
        let stripped = text.replace("```", "");
        let parts: Vec<&str> = stripped.split('`').collect();
        let mut out = Vec::new();
        for (i, part) in parts.iter().enumerate() {
            if i % 2 == 1 {
                let name: String = part
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                if let Some(first) = name.chars().next()
                    && (first.is_ascii_alphabetic() || first == '_')
                {
                    out.push(name);
                }
            }
        }
        out
    }

    #[test]
    fn coding_roster_lists_expected_file_helpers() {
        let roster = coding_roster(&coding_tools_section());
        for expected in ["bash", "read_file", "write_file", "edit_file", "grep", "find", "ls"] {
            assert!(
                roster.contains(expected),
                "coding_tools_section() no longer documents `{expected}` — prompt/prelude drift"
            );
        }
    }

    #[test]
    fn ported_sections_only_name_real_prelude_helpers() {
        let roster = coding_roster(&coding_tools_section());
        // Core REPL builtins the prompt legitimately references (from
        // DEFAULT_REPL_TOOLS, not the coding prelude) — allowlisted so they
        // don't trip the "must be a coding helper" assertion.
        let allowlist: HashSet<&str> = [
            "rlm_query",
            "rlm_query_batched",
            "llm_query",
            "llm_query_batched",
        ]
        .iter()
        .copied()
        .collect();

        for (label, section) in [
            ("DOING_TASKS", DEFAULT_DOING_TASKS),
            ("USING_YOUR_TOOLS", DEFAULT_USING_YOUR_TOOLS),
            ("TONE_AND_STYLE", DEFAULT_TONE_AND_STYLE),
            ("ACTIONS", DEFAULT_ACTIONS),
        ] {
            for helper in backticked_helpers(section) {
                assert!(
                    roster.contains(&helper) || allowlist.contains(helper.as_str()),
                    "{label} references `{helper}` which is neither a coding prelude helper \
                     nor an allowlisted core REPL builtin — prompt/prelude drift"
                );
            }
        }

        // The dedicated file helpers must still be recommended (no accidental
        // removal), not merely rostered.
        for expected in ["read_file", "write_file", "edit_file", "grep", "find", "bash"] {
            assert!(
                DEFAULT_USING_YOUR_TOOLS.contains(expected),
                "USING_YOUR_TOOLS no longer names `{expected}`"
            );
        }
    }
}
