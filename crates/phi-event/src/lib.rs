use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The kind of lifecycle event that was emitted.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum EventKind {
    // ── Core lifecycle ──────────────────────────────────────────────────────
    RuntimeStart,
    SessionStart,
    TuiReady,
    AgentStart,
    AgentEnd,
    TurnStart,
    TurnEnd,
    BeforeAgentStart,
    Context,
    ToolCall,
    ToolResult,
    Input,
    SessionShutdown,

    // ── Project trust ────────────────────────────────────────────────────────
    ProjectTrust,

    // ── Resource discovery ────────────────────────────────────────────────────
    ResourcesDiscover,

    // ── Session lifecycle (interception-capable) ──────────────────────────────
    SessionBeforeSwitch,
    SessionBeforeFork,
    SessionBeforeCompact,
    SessionCompact,
    SessionBeforeTree,
    SessionTree,

    // ── Provider events ───────────────────────────────────────────────────────
    BeforeProviderRequest,
    AfterProviderResponse,

    // ── Message streaming ────────────────────────────────────────────────────
    MessageStart,
    MessageUpdate,
    MessageEnd,

    // ── Tool execution ────────────────────────────────────────────────────────
    ToolExecutionStart,
    ToolExecutionUpdate,
    ToolExecutionEnd,

    // ── Model selection ───────────────────────────────────────────────────────
    ModelSelect,
    ThinkingLevelSelect,

    // ── User bash ─────────────────────────────────────────────────────────────
    UserBash,
}

/// The payload accompanying a lifecycle event.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum EventPayload {
    // ── Empty / generic ──────────────────────────────────────────────────────
    Empty,

    // ── Session lifecycle ──────────────────────────────────────────────────────
    SessionStart {
        cwd: String,
        session_id: String,
        #[serde(default)]
        reason: Option<String>,
    },
    SessionShutdown {
        #[serde(default)]
        reason: Option<String>,
    },

    // ── Agent lifecycle ────────────────────────────────────────────────────────
    BeforeAgentStart {
        prompt: String,
        system_prompt: String,
    },
    AgentStart,
    AgentEnd {
        stop_cause: String,
        /// Bounded (≤4K chars) model-authored completion report set via
        /// `answer['summary']` (WS7.3); `None` when the run didn't set one.
        #[serde(default)]
        summary: Option<String>,
    },

    // ── Turn lifecycle ────────────────────────────────────────────────────────
    TurnStart {
        turn: u32,
    },
    TurnEnd {
        turn: u32,
        had_code: bool,
    },

    // ── Context injection ──────────────────────────────────────────────────────
    Context {
        turn: u32,
    },

    // ── Tool events ────────────────────────────────────────────────────────────
    ToolCall {
        name: String,
        args: Value,
    },
    ToolResult {
        name: String,
        result: String,
        error: Option<String>,
    },
    ToolExecutionStart {
        tool_call_id: String,
        tool_name: String,
        args: Value,
    },
    ToolExecutionUpdate {
        tool_call_id: String,
        tool_name: String,
        partial_result: Value,
    },
    ToolExecutionEnd {
        tool_call_id: String,
        tool_name: String,
        result: Value,
        is_error: bool,
    },

    // ── Input ────────────────────────────────────────────────────────────────
    Input {
        text: String,
        #[serde(default)]
        source: Option<String>,
    },

    // ── Project trust ────────────────────────────────────────────────────────
    ProjectTrust {
        cwd: String,
    },

    // ── Resource discovery ────────────────────────────────────────────────────
    ResourcesDiscover {
        cwd: String,
        reason: String,
    },

    // ── Session interception ──────────────────────────────────────────────────
    SessionBeforeSwitch {
        reason: String,
        target_session_file: Option<String>,
    },
    SessionBeforeFork {
        entry_id: String,
        position: String,
    },
    SessionBeforeCompact {
        custom_instructions: Option<String>,
    },
    SessionCompact {
        from_extension: bool,
    },
    SessionBeforeTree {
        target_id: String,
        old_leaf_id: Option<String>,
    },
    SessionTree {
        new_leaf_id: Option<String>,
        old_leaf_id: Option<String>,
    },

    // ── Provider events ───────────────────────────────────────────────────────
    BeforeProviderRequest {
        payload: Value,
    },
    AfterProviderResponse {
        status: u16,
        headers: Value,
    },

    // ── Message streaming ────────────────────────────────────────────────────
    MessageStart {
        message: Value,
    },
    MessageUpdate {
        message: Value,
        event: Value,
    },
    MessageEnd {
        message: Value,
    },

    // ── Model selection ───────────────────────────────────────────────────────
    ModelSelect {
        model: String,
        previous_model: Option<String>,
        source: String,
    },
    ThinkingLevelSelect {
        level: String,
        previous_level: String,
    },

    // ── User bash ─────────────────────────────────────────────────────────────
    UserBash {
        command: String,
        exclude_from_context: bool,
        cwd: String,
    },
}

/// A lifecycle event dispatched to extension handlers.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LifecycleEvent {
    pub kind: EventKind,
    pub payload: EventPayload,
}

impl LifecycleEvent {
    /// Construct an event with an empty payload.
    pub fn empty(kind: EventKind) -> Self {
        Self {
            kind,
            payload: EventPayload::Empty,
        }
    }
}

/// The value returned by an extension event handler.
///
/// Handlers return `None` to observe-only, or `Some(EventReturn::...)` to
/// influence behavior. The variant must match the event kind — mismatched
/// returns are silently ignored by the runtime.
#[derive(Clone, Debug)]
pub enum EventReturn {
    // ── System prompt modification ────────────────────────────────────────────
    /// Returned for [`EventKind::BeforeAgentStart`].
    ///
    /// `prepend` / `append` inject text around the existing system prompt;
    /// `replace` substitutes it entirely. `message` injects an additional
    /// system-level message alongside the prompt.
    SystemPromptMod {
        append: Option<String>,
        prepend: Option<String>,
        replace: Option<String>,
    },

    // ── Context injection ──────────────────────────────────────────────────────
    MessageInjection {
        prepend: Vec<(String, String)>,
    },

    // ── Tool interception ──────────────────────────────────────────────────────
    /// Returned for [`EventKind::ToolCall`] — block the tool from executing.
    ToolBlock {
        reason: String,
    },
    /// Returned for [`EventKind::ToolCall`] — rewrite the tool arguments
    /// before execution. The runtime replaces the call's `args` with the
    /// provided value and proceeds normally.
    ToolArgsOverride {
        args: Value,
    },
    /// Returned for [`EventKind::ToolResult`] — replace the tool's output.
    /// `is_error` marks the override as an error result; `details` carries
    /// optional structured data.
    ToolResultOverride {
        content: String,
        is_error: Option<bool>,
        details: Option<Value>,
    },

    // ── Input action ──────────────────────────────────────────────────────────
    /// Returned for [`EventKind::Input`]. `text` replaces the user prompt;
    /// `images` attaches image inputs alongside the text.
    InputAction {
        action: InputActionKind,
        text: Option<String>,
        images: Option<Vec<Value>>,
    },

    // ── Session interception (cancel or allow) ────────────────────────────────
    Cancel {
        reason: String,
    },
    /// Returned for [`EventKind::SessionBeforeFork`] — cancel the fork and
    /// optionally skip conversation history restore in the new branch.
    SessionBeforeForkCancel {
        reason: String,
        skip_conversation_restore: bool,
    },
    /// Returned for [`EventKind::SessionBeforeTree`] — cancel the tree
    /// navigation and optionally override the summary shown.
    SessionBeforeTreeCancel {
        reason: String,
        summary: Option<String>,
    },

    // ── Provider request modification ──────────────────────────────────────────
    ProviderRequestOverride {
        payload: Value,
    },

    // ── Resource discovery result ──────────────────────────────────────────────
    ResourcesDiscovered {
        skill_paths: Vec<String>,
        prompt_paths: Vec<String>,
        theme_paths: Vec<String>,
    },

    // ── Project trust result ────────────────────────────────────────────────────
    ProjectTrustResult {
        trusted: bool,
        remember: bool,
    },

    // ── Compaction customization ───────────────────────────────────────────────
    /// Returned for [`EventKind::SessionBeforeCompact`]. `cancel` aborts
    /// compaction; `compaction` provides a pre-computed result so the
    /// runtime skips its own compaction logic; otherwise `instructions`
    /// customize the compaction prompt.
    CompactCustom {
        instructions: String,
        cancel: bool,
        compaction: Option<Value>,
    },

    // ── Message modification ──────────────────────────────────────────────────
    MessageReplace {
        message: Value,
    },
}

/// The kind of input action requested by an event handler.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum InputActionKind {
    Continue,
    Transform,
    Handled,
}

/// A handler function invoked when a lifecycle event is dispatched.
pub type EventHandler = Arc<dyn Fn(LifecycleEvent) -> Result<Option<EventReturn>> + Send + Sync>;

/// A handler that also receives an extension context.
pub type ContextualEventHandler =
    Arc<dyn Fn(LifecycleEvent, ExtensionContextRef) -> Result<Option<EventReturn>> + Send + Sync>;

/// A lightweight, cloneable reference to the extension context.
///
/// This is a stub that will be filled in by the runtime. For now it carries
/// the cwd and session id; additional capabilities will be wired through as
/// the runtime grows.
#[derive(Clone, Debug)]
pub struct ExtensionContextRef {
    pub cwd: String,
    pub session_id: String,
    pub mode: ExtensionMode,
    pub has_ui: bool,
}

/// The mode the extension runtime is operating in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExtensionMode {
    Tui,
    Rpc,
    Print,
}

#[derive(Deserialize)]
pub struct EventHandlerRegistration {
    pub event: EventKind,
    pub label: String,
    #[serde(skip, default = "default_handler")]
    pub handler: EventHandler,
    /// Per-handler override of the dispatch watchdog budget. `None` uses the
    /// dispatcher's per-kind default. A handler that legitimately waits on
    /// the user (e.g. an approval gate blocking on a UI prompt) raises this
    /// above the tight interception default; the watchdog itself always
    /// exists (doctrine 2) — there is deliberately no "unbounded" option.
    #[serde(skip)]
    pub timeout: Option<std::time::Duration>,
}

fn default_handler() -> EventHandler {
    Arc::new(|_event| Ok(None))
}

impl std::fmt::Display for EventKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::RuntimeStart => "runtime_start",
            Self::SessionStart => "session_start",
            Self::TuiReady => "tui_ready",
            Self::AgentStart => "agent_start",
            Self::AgentEnd => "agent_end",
            Self::TurnStart => "turn_start",
            Self::TurnEnd => "turn_end",
            Self::BeforeAgentStart => "before_agent_start",
            Self::Context => "context",
            Self::ToolCall => "tool_call",
            Self::ToolResult => "tool_result",
            Self::Input => "input",
            Self::SessionShutdown => "session_shutdown",
            Self::ProjectTrust => "project_trust",
            Self::ResourcesDiscover => "resources_discover",
            Self::SessionBeforeSwitch => "session_before_switch",
            Self::SessionBeforeFork => "session_before_fork",
            Self::SessionBeforeCompact => "session_before_compact",
            Self::SessionCompact => "session_compact",
            Self::SessionBeforeTree => "session_before_tree",
            Self::SessionTree => "session_tree",
            Self::BeforeProviderRequest => "before_provider_request",
            Self::AfterProviderResponse => "after_provider_response",
            Self::MessageStart => "message_start",
            Self::MessageUpdate => "message_update",
            Self::MessageEnd => "message_end",
            Self::ToolExecutionStart => "tool_execution_start",
            Self::ToolExecutionUpdate => "tool_execution_update",
            Self::ToolExecutionEnd => "tool_execution_end",
            Self::ModelSelect => "model_select",
            Self::ThinkingLevelSelect => "thinking_level_select",
            Self::UserBash => "user_bash",
        };

        f.write_str(name)
    }
}
