use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// A slash command contributed through the extension API. Built-in commands
/// register through this same spec from `phi-builtins` — there is no separate
/// built-in command path.
#[derive(Clone, Serialize)]
pub struct CommandSpec {
    pub name: String,
    pub description: String,
    /// Usage hint rendered after the name in completion and help,
    /// e.g. `"[provider]"`. Empty for commands without arguments.
    pub args_hint: String,
    #[serde(skip_serializing)]
    pub handler: CommandHandler,
    /// Completion candidates for the command's first argument, queried by
    /// the frontend when the user types `/name <prefix>`.
    #[serde(skip_serializing)]
    pub arg_completions: Option<ArgCompletionFn>,
}

pub type ArgCompletionFn = Arc<dyn Fn() -> Vec<String> + Send + Sync>;

/// Listing entry for completion popups and help output.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommandInfo {
    pub name: String,
    pub args_hint: String,
    pub description: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommandInvocation {
    pub raw_args: String,
}

/// A declarative host-side effect requested by a command handler. Handlers
/// run off the UI thread and cannot touch frontend state directly; instead
/// they return actions that the frontend (TUI, RPC, ...) interprets. This is
/// the command API's `ctx`: anything a built-in needs that isn't expressible
/// here is an API gap to be filled by a new variant, not a hardwired branch.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "data")]
pub enum UiAction {
    /// Exit the frontend.
    Quit,
    /// Reset to a fresh session (new store, cleared REPL and history).
    NewSession,
    /// Open one of the host's pickers.
    OpenPicker(PickerKind),
    /// Switch the root (or sub) model to `query`, a registry lookup string.
    SwitchModel { root: bool, query: String },
    /// Begin an interactive OAuth login. Empty provider = current root provider.
    StartLogin { provider: String },
    /// Remove stored credentials. Empty provider = current root provider.
    Logout { provider: String },
    /// Render the host's command and keybinding help.
    ShowHelp,
    /// Persist a project-trust decision for the current project. Takes
    /// effect for project-local extensions on the next session build.
    TrustProject { trusted: bool },
}

/// Pickers a frontend is expected to provide.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum PickerKind {
    Resume,
    Fork,
    Tree,
    Usage,
    Model { root: bool },
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CommandOutput {
    pub message: Option<String>,
    #[serde(default)]
    pub actions: Vec<UiAction>,
}

impl CommandOutput {
    pub fn message(msg: impl Into<String>) -> Self {
        Self {
            message: Some(msg.into()),
            actions: Vec::new(),
        }
    }

    pub fn action(action: UiAction) -> Self {
        Self {
            message: None,
            actions: vec![action],
        }
    }
}

pub type CommandHandler = Arc<dyn Fn(CommandInvocation) -> Result<CommandOutput> + Send + Sync>;
