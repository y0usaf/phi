//! Layered Lua configuration (pi's settings.json, XDG-flavored): a global
//! file at `$XDG_CONFIG_HOME/phi/config.lua` with per-project overrides in
//! `.phi/config.lua`, and CLI flags taking precedence over both. Each file
//! is a Lua chunk returning a settings table — the same language planned
//! for third-party extensions (phi-lua), nvim-style.

use std::path::Path;

use anyhow::{Context as _, Result};
use mlua::LuaSerdeExt as _;
use phi_cli::Args;
use serde::Deserialize;

#[derive(Debug, Default, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub model: Option<String>,
    pub sub_model: Option<String>,
    pub thinking: Option<String>,
    pub python: Option<String>,
    pub max_iterations: Option<usize>,
    pub max_depth: Option<usize>,
    pub budget: Option<f64>,
    pub max_tokens: Option<u64>,
    pub timeout: Option<u64>,
    pub max_errors: Option<usize>,
    pub max_concurrent_subcalls: Option<usize>,
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
            python,
            max_iterations,
            max_depth,
            budget,
            max_tokens,
            timeout,
            max_errors,
            max_concurrent_subcalls
        );
    }
}

/// Final values after applying CLI flags over config over built-in defaults.
pub struct Settings {
    pub model: String,
    pub sub_model: Option<String>,
    pub thinking: String,
    pub python: String,
    pub max_iterations: usize,
    pub max_depth: usize,
    pub budget: Option<f64>,
    pub max_tokens: Option<u64>,
    pub timeout: Option<u64>,
    pub max_errors: Option<usize>,
    pub max_concurrent_subcalls: usize,
}

impl Settings {
    pub fn resolve(args: &Args, config: Config, tui_mode: bool) -> Self {
        Self {
            model: args
                .model
                .clone()
                .or(config.model)
                .unwrap_or_else(|| "claude-opus-4-7".to_owned()),
            sub_model: args.sub_model.clone().or(config.sub_model),
            thinking: args
                .thinking
                .clone()
                .or(config.thinking)
                .unwrap_or_else(|| "off".to_owned()),
            python: args
                .python
                .clone()
                .or(config.python)
                .unwrap_or_else(|| "python3".to_owned()),
            max_iterations: args.max_iterations.or(config.max_iterations).unwrap_or(30),
            max_depth: args
                .max_depth
                .or(config.max_depth)
                .unwrap_or(if tui_mode { 3 } else { 2 }),
            budget: args.budget.or(config.budget),
            max_tokens: args.max_tokens.or(config.max_tokens),
            timeout: args.timeout.or(config.timeout),
            max_errors: args.max_errors.or(config.max_errors),
            max_concurrent_subcalls: args
                .max_concurrent_subcalls
                .or(config.max_concurrent_subcalls)
                .unwrap_or(4),
        }
    }
}

#[cfg(test)]
mod tests {
    use phi_cli::clap::Parser as _;

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
    fn max_depth_default_depends_on_mode() {
        let args = Args::parse_from(["phi"]);
        assert_eq!(
            Settings::resolve(&args, Config::default(), true).max_depth,
            3
        );
        assert_eq!(
            Settings::resolve(&args, Config::default(), false).max_depth,
            2
        );

        let config = eval("return { max_depth = 5 }").unwrap();
        assert_eq!(Settings::resolve(&args, config, true).max_depth, 5);

        let args = Args::parse_from(["phi", "--max-depth", "7"]);
        assert_eq!(
            Settings::resolve(&args, Config::default(), true).max_depth,
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
}
