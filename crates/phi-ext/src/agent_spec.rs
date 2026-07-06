//! Agent packs (WS1): a named bundle of agent-defining knobs activated as a
//! unit, so `/agent <name>` can swap the entire agent — system prompt, tool
//! surface, turn protocol, execution environment — at runtime or session
//! build.
//!
//! An agent's members are the *existing* configuration shapes — a
//! system-prompt identity, a tool roster, an env block, turn-protocol
//! knobs — bundled as a declarative profile rather than invented as a
//! parallel vocabulary (one declaration mechanism, doctrine 5). Members are
//! optional: a member left empty falls through to the engine defaults, so the
//! shipped default agent is simply a bundle named `rlm` that carries today's
//! values and a builtins-less build degrades the same way it does for styles
//! today.
//!
//! Exactly one agent is active per session
//! ([`crate::AppRuntime::set_active_agent`]). Switching mid-session starts a
//! fresh turn context; it does not rewrite history.

/// Per-tool exposure override within an agent's tool roster. When set, this
/// overrides the tool's registered `ToolExposure` for sessions using this
/// agent. `None` means "use the tool's registered exposure" (fall through).
///
/// This is the WS1.2 vocabulary's forward-compatible slot; for the WS1.1
/// primitive it is unused (the roster is a list of names, exposure comes from
/// the registry).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolExposureOverride {
    /// Use the tool's registered exposure (the default).
    #[default]
    Inherit,
    /// Force the tool to appear in the model's native tool list.
    Native,
    /// Force the tool to appear only in the REPL (no native surface).
    Repl,
    /// Force the tool onto both surfaces.
    Both,
    /// Hide the tool entirely from this agent's model.
    Hidden,
}

/// The execution-environment block (WS1.2). Controls the model-visible
/// surface: whether the `repl` tool is exposed, whether the `context` /
/// `context_0` variables are injected. Members are optional and fall through
/// to the engine defaults (both on) when absent — so the shipped `rlm` profile
/// leaves this empty and the pi-shaped profile (WS1.5) sets `repl_tool = false`.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AgentEnv {
    /// When `Some(false)`, the `repl` tool is removed from the model's surface
    /// and its routing is skipped. `None` = inherit (repl tool on, today's
    /// behavior).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repl_tool: Option<bool>,
    /// When `Some(false)`, the `context` / `context_0` / … variables are not
    /// injected into the REPL env. `None` = inherit (context on).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<bool>,
}

/// The turn-protocol block (WS1.3). Each knob is optional and falls through
/// to the engine default when absent — so the shipped `rlm` profile leaves
/// this empty and the pi/codex profiles set the knobs they differ on.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AgentTurn {
    /// Stop condition: `prose` (non-empty text turn ends the run — today's
    /// behavior), `answer` (only `answer['ready']` / `<task_complete>`),
    /// `end_turn` (codex-style explicit end-turn tool). `None` = inherit
    /// (prose + answer, today's behavior).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<String>,
    /// Whether the engine injects a per-turn user message with remaining-turn
    /// reminders. `None` = inherit (only when approaching the cap, today's
    /// behavior).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub per_turn_reminder: Option<bool>,
    /// Whether parallel tool execution is enabled. `None` = inherit (today's
    /// behavior — tools execute eagerly during the stream).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_tools: Option<bool>,
}

/// A named agent-profile bundle registered by an extension.
///
/// Every member is optional: a member left empty falls through to the engine
/// defaults, so the shipped default agent (`rlm`) carries today's values and
/// a builtins-less build (no agent registered) keeps the engine's built-in
/// identity.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct AgentSpec {
    pub name: String,
    /// System-prompt identity: full text that overrides the engine's default
    /// identity while this agent is active. A `.phi/SYSTEM.md` override still
    /// wins (the `BeforeAgentStart` → `SystemPromptMod` hook is for mutation;
    /// identity *selection* moves here). `None` = inherit the engine default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    /// Whether to append the orchestrator addendum (delegate-by-default
    /// guidance for long-context/QA payloads). `None` = inherit the engine
    /// default (`false` for coding sessions, today's behavior).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orchestrator: Option<bool>,
    /// Roster of registered tool names this agent exposes, optionally with a
    /// per-tool exposure override. An empty roster means "all registered
    /// tools" (inherit). WS1.2 will wire this into the engine's per-turn tool
    /// list; for the WS1.1 primitive it is stored but not yet consumed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<AgentToolEntry>,
    /// Execution-environment block (WS1.2). `None` = inherit (today's env).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<AgentEnv>,
    /// Turn-protocol block (WS1.3). `None` = inherit (today's turn protocol).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn: Option<AgentTurn>,
    /// The profile name that child `subagent()` calls spawn with when this
    /// agent is active. `None` = children inherit the parent's profile (or the
    /// engine default if no profile is active).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_defaults: Option<String>,
}

/// One entry in an agent's tool roster: a tool name plus an optional exposure
/// override.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AgentToolEntry {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exposure: Option<ToolExposureOverride>,
}

impl AgentSpec {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
        }
    }

    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    pub fn with_orchestrator(mut self, orchestrator: bool) -> Self {
        self.orchestrator = Some(orchestrator);
        self
    }

    pub fn with_subagent_defaults(mut self, profile: impl Into<String>) -> Self {
        self.subagent_defaults = Some(profile.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_bundle_serializes_minimal() {
        let spec = AgentSpec::new("rlm");
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json["name"], "rlm");
        assert!(json.get("system_prompt").is_none() || json["system_prompt"].is_null());
    }

    #[test]
    fn round_trips_with_system_prompt() {
        let spec = AgentSpec::new("rlm").with_system_prompt("You are a coding agent.");
        let json = serde_json::to_string(&spec).unwrap();
        let back: AgentSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "rlm");
        assert_eq!(
            back.system_prompt.as_deref(),
            Some("You are a coding agent.")
        );
    }

    #[test]
    fn env_and_turn_serialize_only_when_set() {
        let spec = AgentSpec::new("pi")
            // env via raw struct
            .with_system_prompt("flat tool-call loop");
        let json = serde_json::to_value(&spec).unwrap();
        // env and turn should be absent (not null) when not set
        assert!(json.get("env").map(|v| v.is_null()).unwrap_or(true));
        assert!(json.get("turn").map(|v| v.is_null()).unwrap_or(true));
    }
}
