use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Which frontend surface a command belongs to. Context-tags commands so the
/// mux `:` command line only completes/dispatches `Mux`-context commands and
/// the session TUI's `/` command line only `Session`-context ones.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub enum CommandContext {
    /// Session TUI slash commands (`/model`, `/login`, ...). The default.
    #[default]
    Session,
    /// Mux colon commands (`:open`, `:quit`, ...).
    Mux,
}

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
    /// Which frontend surface dispatches this command.
    pub context: CommandContext,
    /// Alternative names resolved by the registry within the same context.
    /// Kept out of primary listings (completion, help).
    pub aliases: Vec<String>,
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
    /// Open a named overlay (built-in or extension-registered) — the
    /// generic escape hatch for host-rendered modals. The host interprets
    /// well-known names specially (e.g. `OVERLAY_MODEL` reads
    /// `{"root": bool}` from `payload` and attaches the model list and
    /// response channel itself); unknown names are routed to the
    /// open-overlay-by-name path with the JSON payload forwarded to the
    /// overlay's `init_state`.
    OpenOverlay {
        name: String,
        payload: Option<serde_json::Value>,
    },
    /// Switch the root (or sub) model to `query`, a registry lookup string.
    SwitchModel { root: bool, query: String },
    /// Begin an interactive OAuth login. Empty provider = current root provider.
    StartLogin { provider: String },
    /// Remove stored credentials. Empty provider = current root provider.
    Logout { provider: String },
    /// Persist a project-trust decision for the current project. Takes
    /// effect for project-local extensions on the next session build.
    TrustProject { trusted: bool },
    /// Activate a registered style pack (WS10) and persist the choice.
    /// Empty name: report the available styles instead of switching. The
    /// host re-resolves its visuals (palette, spinner, tool renderers) and
    /// repaints.
    SwitchStyle { name: String },
    /// Activate a registered agent profile (WS1) and persist the choice.
    /// Empty name: report the available agents instead of switching. The
    /// host re-resolves the agent's knobs (system prompt, tool surface, turn
    /// protocol) and applies them to the next turn — switching mid-session
    /// starts a fresh turn context, it does not rewrite history.
    SwitchAgent { name: String },
    /// Add the directory as a project in the mux sidebar and focus it — a
    /// genuinely mux-host-owned effect (workspace/project management).
    OpenProject(std::path::PathBuf),
    /// Generic namespaced host action, a mux-host-owned effect: the host maps
    /// known names (e.g. `"refresh"`, `"session-next"`) onto its own
    /// behaviors; unknown names produce an error note.
    RunHostAction { name: String },
    /// Inject `text` as a user message and immediately start a model turn —
    /// the "prompt-type" command effect. This is the host side of skill
    /// slash commands: a `/skill-name` handler returns the SKILL.md body as
    /// a `SendPrompt`, and the session TUI routes it through the same
    /// `send_prompt` path as a typed prompt. The mux host (which has no
    /// active session turn) reports it as unsupported, like the other
    /// session-only actions.
    SendPrompt { text: String },
}

/// Pickers a frontend is expected to provide. Only genuinely host-owned
/// pickers live here; overlays that extensions register (usage, model,
/// help, ...) are opened by name via [`UiAction::OpenOverlay`].
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum PickerKind {
    Resume,
    Fork,
    Tree,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
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
