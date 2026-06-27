//! Layered Lua configuration (pi's settings.json, XDG-flavored): a global
//! file at `$XDG_CONFIG_HOME/phi/config.lua` with per-project overrides in
//! `.phi/config.lua`, and CLI flags taking precedence over both. Each file
//! is a Lua chunk returning a settings table — the same language planned
//! for third-party extensions (phi-lua), nvim-style.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context as _, Result};
use mlua::LuaSerdeExt as _;

use serde::Deserialize;

/// Generates a struct with the twelve fields shared across [`Config`],
/// [`CliArgs`], and [`Settings`], plus any extra trailing fields. Each
/// invocation supplies its own resolved types, so adding or renaming a
/// shared field here propagates to all three structs automatically.
macro_rules! shared_fields_struct {
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident {
            model: $model_ty:ty,
            sub_model: $sub_model_ty:ty,
            thinking: $thinking_ty:ty,
            sub_model_thinking: $sub_model_thinking_ty:ty,
            python: $python_ty:ty,
            max_iterations: $max_iterations_ty:ty,
            max_rlm_depth: $max_rlm_depth_ty:ty,
            budget: $budget_ty:ty,
            max_tokens: $max_tokens_ty:ty,
            timeout: $timeout_ty:ty,
            max_errors: $max_errors_ty:ty,
            max_concurrent_subcalls: $max_concurrent_subcalls_ty:ty,
            $($extra:tt)*
        }
    ) => {
        $(#[$meta])*
        $vis struct $name {
            pub model: $model_ty,
            pub sub_model: $sub_model_ty,
            pub thinking: $thinking_ty,
            pub sub_model_thinking: $sub_model_thinking_ty,
            pub python: $python_ty,
            pub max_iterations: $max_iterations_ty,
            pub max_rlm_depth: $max_rlm_depth_ty,
            pub budget: $budget_ty,
            pub max_tokens: $max_tokens_ty,
            pub timeout: $timeout_ty,
            pub max_errors: $max_errors_ty,
            pub max_concurrent_subcalls: $max_concurrent_subcalls_ty,
            $($extra)*
        }
    };
}

shared_fields_struct! {
    #[derive(Debug, Default, Clone, Deserialize, PartialEq)]
    #[serde(default, deny_unknown_fields)]
    pub struct Config {
        model: Option<String>,
        sub_model: Option<String>,
        thinking: Option<String>,
        sub_model_thinking: Option<String>,
        python: Option<String>,
        max_iterations: Option<usize>,
        max_rlm_depth: Option<usize>,
        budget: Option<f64>,
        max_tokens: Option<u64>,
        timeout: Option<u64>,
        max_errors: Option<usize>,
        max_concurrent_subcalls: Option<usize>,
        pub mux: Option<MuxConfig>,
        /// User-configured skill directories or files to scan for `SKILL.md`.
        /// Paths support `~` expansion; relative paths resolve against `cwd`.
        /// Example: `skills = { "~/.claude/skills", ".forge/skills" }`
        pub skills: Option<Vec<String>>,
    }
}

/// Settings for mux mode (the in-process session multiplexer).
#[derive(Debug, Default, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct MuxConfig {
    pub enabled: Option<bool>,
    /// Action name → single keybinding string (e.g. `new_session = "ctrl+n"`,
    /// `quit = "ctrl+q"`). Overrides the defaults per action.
    pub keybinds: Option<HashMap<String, String>>,
    pub sidebar_width: Option<u16>,
}

impl Config {
    /// Global config overlaid with the project's `.phi/config.lua`.
    pub fn load(cwd: &Path) -> Result<Self> {
        let mut config = Self::default();
        if let Some(dirs) = directories::ProjectDirs::from("", "", "phi") {
            config.overlay(Self::read(&dirs.config_dir().join("config.lua"))?);
        }
        config.overlay(Self::read(&cwd.join(".phi").join("config.lua"))?);
        Ok(config)
    }

    fn read(path: &Path) -> Result<Self> {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Ok(Self::default());
        };
        Self::eval(&text, &path.display().to_string())
            .with_context(|| format!("invalid config: {}", path.display()))
    }

    /// Evaluate a Lua chunk that returns a settings table.
    fn eval(text: &str, name: &str) -> Result<Self> {
        let lua = mlua::Lua::new();
        // No-op phi global: config.lua may contain phi.* calls (extension entry point).
        // The real phi global is set up by LuaExtension when loaded as an extension.
        {
            let noop = lua
                .create_function(|_, _: mlua::MultiValue| Ok(()))
                .expect("noop fn");
            let phi_table = lua.create_table().expect("phi table");
            for method in &["on", "register_tool", "register_command"] {
                phi_table
                    .set(*method, noop.clone())
                    .expect("set phi method");
            }
            lua.globals().set("phi", phi_table).expect("set phi global");
        }
        let value: mlua::Value = lua.load(text).set_name(format!("@{name}")).eval()?;
        if value.is_nil() {
            anyhow::bail!("chunk must return a settings table");
        }
        Ok(lua.from_value(value)?)
    }

    fn overlay(&mut self, other: Self) {
        macro_rules! take {
            ($($field:ident),*) => {
                $(if other.$field.is_some() { self.$field = other.$field; })*
            };
        }
        take!(
            model,
            sub_model,
            thinking,
            sub_model_thinking,
            python,
            max_iterations,
            max_rlm_depth,
            budget,
            max_tokens,
            timeout,
            max_errors,
            max_concurrent_subcalls,
            skills
        );
        if let Some(other_mux) = other.mux {
            let mux = self.mux.get_or_insert_with(MuxConfig::default);
            macro_rules! take_mux {
                ($($field:ident),*) => {
                    $(if other_mux.$field.is_some() { mux.$field = other_mux.$field; })*
                };
            }
            take_mux!(enabled, keybinds, sidebar_width);
        }
    }
}

shared_fields_struct! {
    /// CLI argument fields needed to resolve [`Settings`], decoupled from clap so
    /// phi-session doesn't depend on a CLI parser.
    #[derive(Debug, Default, Clone)]
    pub struct CliArgs {
        model: Option<String>,
        sub_model: Option<String>,
        thinking: Option<String>,
        sub_model_thinking: Option<String>,
        python: Option<String>,
        max_iterations: Option<usize>,
        max_rlm_depth: Option<usize>,
        budget: Option<f64>,
        max_tokens: Option<u64>,
        timeout: Option<u64>,
        max_errors: Option<usize>,
        max_concurrent_subcalls: Option<usize>,
    }
}

shared_fields_struct! {
    /// Final values after applying CLI flags over config over built-in defaults.
    pub struct Settings {
        model: String,
        sub_model: Option<String>,
        thinking: String,
        sub_model_thinking: String,
        python: String,
        max_iterations: usize,
        max_rlm_depth: usize,
        budget: Option<f64>,
        max_tokens: Option<u64>,
        timeout: Option<u64>,
        max_errors: Option<usize>,
        max_concurrent_subcalls: usize,
        /// User-configured skill paths (from config.lua `skills` field).
        pub skills: Vec<String>,
    }
}

impl Settings {
    pub fn resolve(args: &CliArgs, config: Config, tui_mode: bool) -> Self {
        let thinking = args
            .thinking
            .clone()
            .or(config.thinking.clone())
            .unwrap_or_else(|| "off".to_owned());
        Self {
            model: args
                .model
                .clone()
                .or(config.model)
                .unwrap_or_else(|| "claude-opus-4-7".to_owned()),
            sub_model: args.sub_model.clone().or(config.sub_model),
            thinking: thinking.clone(),
            sub_model_thinking: args
                .sub_model_thinking
                .clone()
                .or(config.sub_model_thinking)
                .unwrap_or_else(|| thinking.clone()),
            python: args
                .python
                .clone()
                .or(config.python)
                .unwrap_or_else(|| "python3".to_owned()),
            max_iterations: args.max_iterations.or(config.max_iterations).unwrap_or(30),
            max_rlm_depth: args
                .max_rlm_depth
                .or(config.max_rlm_depth)
                .unwrap_or(if tui_mode { 3 } else { 2 }),
            budget: args.budget.or(config.budget),
            max_tokens: args.max_tokens.or(config.max_tokens),
            timeout: args.timeout.or(config.timeout),
            max_errors: args.max_errors.or(config.max_errors),
            max_concurrent_subcalls: args
                .max_concurrent_subcalls
                .or(config.max_concurrent_subcalls)
                .unwrap_or(4),
            skills: config.skills.unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(text: &str) -> Result<Config> {
        Config::eval(text, "test")
    }

    #[test]
    fn lua_chunk_returns_settings_table() {
        let config = eval(
            "local budget = 1.25\n\
             return { model = \"openai/gpt-5.4\", max_iterations = 10, budget = budget * 2 }",
        )
        .unwrap();
        assert_eq!(config.model.as_deref(), Some("openai/gpt-5.4"));
        assert_eq!(config.max_iterations, Some(10));
        assert_eq!(config.budget, Some(2.5));
    }

    #[test]
    fn project_config_overlays_global() {
        let mut global = eval("return { model = \"a\", max_iterations = 10 }").unwrap();
        let project = eval("return { model = \"b\", timeout = 60 }").unwrap();
        global.overlay(project);
        assert_eq!(global.model.as_deref(), Some("b"));
        assert_eq!(global.max_iterations, Some(10));
        assert_eq!(global.timeout, Some(60));
    }

    #[test]
    fn unknown_keys_and_missing_return_are_rejected() {
        assert!(eval("return { no_such_setting = true }").is_err());
        assert!(eval("model = \"oops\"").is_err());
        assert!(eval("syntax error here").is_err());
    }

    #[test]
    fn max_rlm_depth_default_depends_on_mode() {
        let args = CliArgs::default();
        assert_eq!(
            Settings::resolve(&args, Config::default(), true).max_rlm_depth,
            3
        );
        assert_eq!(
            Settings::resolve(&args, Config::default(), false).max_rlm_depth,
            2
        );

        let config = eval("return { max_rlm_depth = 5 }").unwrap();
        assert_eq!(Settings::resolve(&args, config, true).max_rlm_depth, 5);

        let args = CliArgs {
            max_rlm_depth: Some(7),
            ..Default::default()
        };
        assert_eq!(
            Settings::resolve(&args, Config::default(), true).max_rlm_depth,
            7
        );
    }

    #[test]
    fn missing_files_load_as_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            Config::read(&dir.path().join("config.lua")).unwrap(),
            Config::default()
        );
    }

    #[test]
    fn sub_model_thinking_inherits_thinking_when_unset() {
        let args = CliArgs::default();
        let config = eval("return { thinking = \"high\" }").unwrap();
        let settings = Settings::resolve(&args, config, false);
        assert_eq!(settings.thinking, "high");
        assert_eq!(settings.sub_model_thinking, "high");
    }

    #[test]
    fn sub_model_thinking_can_override_thinking() {
        let args = CliArgs {
            thinking: Some("high".to_owned()),
            sub_model_thinking: Some("low".to_owned()),
            ..Default::default()
        };
        let settings = Settings::resolve(&args, Config::default(), false);
        assert_eq!(settings.thinking, "high");
        assert_eq!(settings.sub_model_thinking, "low");
    }
}
