use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::Mutex as TokioMutex;

use crate::{
    ChromeSpec, CommandContext, CommandInvocation, CommandOutput, CommandSpec,
    EventHandlerRegistration, EventKind, EventPayload, EventReturn, ExtensionContextRef,
    ExtensionManifest, ExtensionMode, FlagSpec, LifecycleEvent, MessageRendererSpec, OverlaySpec,
    ProviderConfig, ShortcutSpec, SpinnerSpec, StyleSpec, TextEffectSpec, ThemeSpec,
    ThinkingExpressionSpec, ToolRenderIndex, ToolRenderSpec, ToolSpec,
};
use anyhow::{Result, anyhow, bail};

use crate::snapshot::{
    EventHandlerSummary, RuntimeSnapshot, ThemeSummary, command_summary, flag_summary,
    overlay_summary, shortcut_summary, spinner_summary, text_effect_summary,
    thinking_expression_summary, tool_summary,
};

pub struct AppRuntime {
    cwd: PathBuf,
    session_id: String,
    extensions: Vec<ExtensionManifest>,
    /// Late-bound ask-the-user channel; the same handle extensions cloned
    /// at `register()` time, so `connect_ui` reaches them all.
    ui: crate::UiHandle,
    pub(crate) tools: BTreeMap<String, ToolSpec>,
    /// Keyed by `(context, name)`: the same name may exist in different
    /// contexts (session `/quit` vs mux `:quit`) without colliding.
    pub(crate) commands: BTreeMap<(CommandContext, String), CommandSpec>,
    pub(crate) shortcuts: Vec<ShortcutSpec>,
    pub(crate) flags: BTreeMap<String, FlagSpec>,
    pub(crate) event_handlers: Vec<EventHandlerRegistration>,
    pub(crate) message_renderers: BTreeMap<String, MessageRendererSpec>,
    pub(crate) tool_renderers: BTreeMap<String, ToolRenderSpec>,
    pub(crate) providers: BTreeMap<String, ProviderConfig>,
    // ── Visual ──────────────────────────────────────────────────────────
    pub(crate) themes: BTreeMap<String, ThemeSpec>,
    pub(crate) spinners: BTreeMap<String, SpinnerSpec>,
    pub(crate) thinking_expressions: BTreeMap<String, ThinkingExpressionSpec>,
    pub(crate) text_effects: BTreeMap<String, TextEffectSpec>,
    pub(crate) overlays: BTreeMap<String, OverlaySpec>,
    pub(crate) styles: BTreeMap<String, StyleSpec>,
    pub(crate) agents: BTreeMap<String, crate::AgentSpec>,
    /// The active style pack (WS10). Interior-mutable like the UI handle:
    /// the registry itself stays immutable after build, but which bundle is
    /// active can change at runtime (`/style`), and hosts re-resolve their
    /// visuals afterwards.
    active_style: Mutex<Option<String>>,
    /// The active agent profile (WS1). Interior-mutable like the active style:
    /// the registry stays immutable after build, but which profile is active
    /// can change at runtime (`/agent`), and the host re-resolves agent knobs
    /// (system prompt, tool surface, turn protocol) afterwards.
    active_agent: Mutex<Option<String>>,
    notifications: Mutex<Vec<String>>,
    status: TokioMutex<BTreeMap<String, String>>,
    session_name: TokioMutex<Option<String>>,
    labels: TokioMutex<BTreeMap<String, String>>,
    active_tools: TokioMutex<Vec<String>>,
    pending_messages: Mutex<Vec<String>>,
}

/// Per-`EventKind` timeout budget for a single extension handler, enforced by
/// [`AppRuntime::dispatch`]. The categories:
/// - **Notifications** (return discarded, often high-frequency on the hot path)
///   get a tight bound so a slow extension can't stall streaming or the agent
///   loop.
/// - **Interception gates** (return drives control flow: input transform,
///   tool block/override, context injection, system-prompt mod, compaction
///   cancel) get a longer bound since their result is consumed.
/// - **One-shot startup/lifecycle** events (may do real work: trust checks,
///   resource discovery) get the longest bound.
fn handler_timeout(kind: EventKind) -> Duration {
    use EventKind::*;
    match kind {
        // High-frequency streaming notifications (returns discarded).
        MessageUpdate
        | ToolExecutionStart
        | ToolExecutionUpdate
        | ToolExecutionEnd
        | MessageStart
        | MessageEnd
        | BeforeProviderRequest
        | AfterProviderResponse
        | ModelSelect
        | ThinkingLevelSelect
        | UserBash
        | TurnStart
        | TurnEnd
        | AgentStart
        | AgentEnd
        | SessionShutdown
        | SessionTree
        | SessionBeforeTree
        | SessionCompact
        | SessionBeforeSwitch
        | SessionBeforeFork => Duration::from_millis(250),
        // Interception gates whose return drives control flow.
        Input | ToolCall | ToolResult | Context | BeforeAgentStart | SessionBeforeCompact => {
            Duration::from_secs(2)
        }
        // One-shot startup/lifecycle (may do real work: trust, resources).
        RuntimeStart | SessionStart | TuiReady | ProjectTrust | ResourcesDiscover => {
            Duration::from_secs(5)
        }
    }
}

impl AppRuntime {
    pub(crate) fn from_builder(
        cwd: PathBuf,
        session_id: String,
        extensions: Vec<ExtensionManifest>,
        ui: crate::UiHandle,
        tools: BTreeMap<String, ToolSpec>,
        commands: BTreeMap<(CommandContext, String), CommandSpec>,
        shortcuts: Vec<ShortcutSpec>,
        flags: BTreeMap<String, FlagSpec>,
        event_handlers: Vec<EventHandlerRegistration>,
        message_renderers: BTreeMap<String, MessageRendererSpec>,
        tool_renderers: BTreeMap<String, ToolRenderSpec>,
        providers: BTreeMap<String, ProviderConfig>,
        themes: BTreeMap<String, ThemeSpec>,
        spinners: BTreeMap<String, SpinnerSpec>,
        thinking_expressions: BTreeMap<String, ThinkingExpressionSpec>,
        text_effects: BTreeMap<String, TextEffectSpec>,
        overlays: BTreeMap<String, OverlaySpec>,
        styles: BTreeMap<String, StyleSpec>,
        agents: BTreeMap<String, crate::AgentSpec>,
    ) -> Self {
        Self {
            cwd,
            session_id,
            extensions,
            ui,
            tools,
            commands,
            shortcuts,
            flags,
            event_handlers,
            message_renderers,
            tool_renderers,
            providers,
            themes,
            spinners,
            thinking_expressions,
            text_effects,
            overlays,
            styles,
            agents,
            active_style: Mutex::new(None),
            active_agent: Mutex::new(None),
            notifications: Mutex::new(Vec::new()),
            status: TokioMutex::new(BTreeMap::new()),
            session_name: TokioMutex::new(None),
            labels: TokioMutex::new(BTreeMap::new()),
            active_tools: TokioMutex::new(Vec::new()),
            pending_messages: Mutex::new(Vec::new()),
        }
    }

    // ── UI ─────────────────────────────────────────────────────────────────────

    /// Attach an interactive host's prompt channel. Extensions that captured
    /// the handle at `register()` time (via `ExtensionHost::ui_handle`) can
    /// then `ask` the user mid-run; until this is called, `has_ui()` is
    /// `false` everywhere and asks resolve to `UiAskOutcome::NoUi`.
    pub fn connect_ui(&self, tx: tokio::sync::mpsc::UnboundedSender<crate::UiPromptRequest>) {
        self.ui.connect(tx);
    }

    /// The runtime's ask-the-user handle (same instance extensions hold).
    pub fn ui_handle(&self) -> crate::UiHandle {
        self.ui.clone()
    }

    // ── Context ────────────────────────────────────────────────────────────────

    /// Build an `ExtensionContextRef` for this runtime.
    pub fn context_ref(&self, mode: ExtensionMode, has_ui: bool) -> ExtensionContextRef {
        ExtensionContextRef {
            cwd: self.cwd.display().to_string(),
            session_id: self.session_id.clone(),
            mode,
            has_ui,
        }
    }

    // ── Async dispatch ────────────────────────────────────────────────────────

    pub async fn dispatch(&self, event: LifecycleEvent) -> Result<Vec<EventReturn>> {
        let mut returns = Vec::new();
        let kind_timeout = handler_timeout(event.kind);
        for reg in self.event_handlers.iter().filter(|h| h.event == event.kind) {
            // A registration may override its own watchdog budget — e.g. an
            // approval gate blocking on a user prompt needs far more than the
            // interception default. The watchdog itself always exists.
            let timeout = reg.timeout.unwrap_or(kind_timeout);
            let h = reg.handler.clone();
            let ev = event.clone();
            // Run each (sync) handler on the blocking pool, but bound the wait
            // so a single slow/blocked extension cannot wedge whichever task
            // dispatched the event. On timeout, error, or join failure we log a
            // notification and CONTINUE to the next handler — one misbehaving
            // extension must not abort dispatch for the rest, nor for the caller.
            // (The spawned blocking task is detached on timeout: its thread keeps
            // running until the handler returns, but we stop waiting on it.)
            match tokio::time::timeout(timeout, tokio::task::spawn_blocking(move || h(ev))).await {
                Ok(Ok(Ok(Some(r)))) => returns.push(r),
                Ok(Ok(Ok(None))) => {}
                Ok(Ok(Err(e))) => {
                    self.push_notification(format!("handler '{}' errored: {e}", reg.label));
                }
                Ok(Err(e)) => {
                    self.push_notification(format!("handler '{}' join error: {e}", reg.label));
                }
                Err(_elapsed) => {
                    self.push_notification(format!(
                        "handler '{}' timed out after {:?}",
                        reg.label, timeout
                    ));
                }
            }
        }
        Ok(returns)
    }

    /// Dispatch an event and check if any handler returned `Cancel`.
    pub async fn dispatch_cancelable(&self, event: LifecycleEvent) -> Result<bool> {
        let returns = self.dispatch(event).await?;
        Ok(returns
            .iter()
            .any(|r| matches!(r, EventReturn::Cancel { .. })))
    }

    // ── System prompt injection ───────────────────────────────────────────────

    pub async fn filter_system_prompt(
        &self,
        prompt: &str,
        system_prompt: String,
    ) -> Result<String> {
        let returns = self
            .dispatch(LifecycleEvent {
                kind: EventKind::BeforeAgentStart,
                payload: EventPayload::BeforeAgentStart {
                    prompt: prompt.to_owned(),
                    system_prompt: system_prompt.clone(),
                },
            })
            .await?;

        let mut result = system_prompt;
        for r in returns {
            if let EventReturn::SystemPromptMod {
                prepend,
                append,
                replace,
            } = r
            {
                if let Some(rep) = replace {
                    result = rep;
                    // After a full replacement, still allow subsequent
                    // prepend/append from other handlers to compose on top.
                    continue;
                }
                if let Some(pre) = prepend {
                    let mut new_result = String::with_capacity(pre.len() + 2 + result.len());
                    new_result.push_str(&pre);
                    new_result.push_str("\n\n");
                    new_result.push_str(&result);
                    result = new_result;
                }
                if let Some(app) = append {
                    result.push_str("\n\n");
                    result.push_str(&app);
                }
            }
        }
        Ok(result)
    }

    // ── Context injection ─────────────────────────────────────────────────────

    pub async fn context_injections(&self, turn: u32) -> Result<Vec<(String, String)>> {
        let returns = self
            .dispatch(LifecycleEvent {
                kind: EventKind::Context,
                payload: EventPayload::Context { turn },
            })
            .await?;

        Ok(returns
            .into_iter()
            .filter_map(|r| {
                if let EventReturn::MessageInjection { prepend } = r {
                    Some(prepend)
                } else {
                    None
                }
            })
            .flatten()
            .collect())
    }

    // ── Resource discovery ────────────────────────────────────────────────────

    pub async fn discover_resources(&self, cwd: &str, reason: &str) -> Result<DiscoveredResources> {
        let returns = self
            .dispatch(LifecycleEvent {
                kind: EventKind::ResourcesDiscover,
                payload: EventPayload::ResourcesDiscover {
                    cwd: cwd.to_owned(),
                    reason: reason.to_owned(),
                },
            })
            .await?;

        let mut resources = DiscoveredResources::default();
        for r in returns {
            if let EventReturn::ResourcesDiscovered {
                skill_paths,
                prompt_paths,
                theme_paths,
            } = r
            {
                resources.skill_paths.extend(skill_paths);
                resources.prompt_paths.extend(prompt_paths);
                resources.theme_paths.extend(theme_paths);
            }
        }
        Ok(resources)
    }

    // ── Project trust ──────────────────────────────────────────────────────────

    pub async fn check_project_trust(&self, cwd: &str) -> Result<ProjectTrustDecision> {
        let returns = self
            .dispatch(LifecycleEvent {
                kind: EventKind::ProjectTrust,
                payload: EventPayload::ProjectTrust {
                    cwd: cwd.to_owned(),
                },
            })
            .await?;

        for r in returns {
            if let EventReturn::ProjectTrustResult { trusted, remember } = r {
                return Ok(ProjectTrustDecision { trusted, remember });
            }
        }
        // No handler weighed in: default to trusted.
        Ok(ProjectTrustDecision {
            trusted: true,
            remember: false,
        })
    }

    // ── Provider request interception ────────────────────────────────────────

    pub async fn before_provider_request(
        &self,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let returns = self
            .dispatch(LifecycleEvent {
                kind: EventKind::BeforeProviderRequest,
                payload: EventPayload::BeforeProviderRequest {
                    payload: payload.clone(),
                },
            })
            .await?;

        let mut result = payload;
        for r in returns {
            if let EventReturn::ProviderRequestOverride {
                payload: override_payload,
            } = r
            {
                result = override_payload;
            }
        }
        Ok(result)
    }

    // ── Session lifecycle interception ──────────────────────────────────────────

    pub async fn before_session_switch(&self, reason: &str, target: Option<&str>) -> Result<bool> {
        self.dispatch_cancelable(LifecycleEvent {
            kind: EventKind::SessionBeforeSwitch,
            payload: EventPayload::SessionBeforeSwitch {
                reason: reason.to_owned(),
                target_session_file: target.map(str::to_owned),
            },
        })
        .await
    }

    /// Check if session fork should proceed. Returns `ForkDecision::Cancel`
    /// if any handler cancelled (via `Cancel` or `SessionBeforeForkCancel`),
    /// otherwise `ForkDecision::Proceed`.
    pub async fn before_session_fork(
        &self,
        entry_id: &str,
        position: &str,
    ) -> Result<ForkDecision> {
        let returns = self
            .dispatch(LifecycleEvent {
                kind: EventKind::SessionBeforeFork,
                payload: EventPayload::SessionBeforeFork {
                    entry_id: entry_id.to_owned(),
                    position: position.to_owned(),
                },
            })
            .await?;

        for r in returns {
            match r {
                EventReturn::Cancel { reason } => {
                    return Ok(ForkDecision::Cancel {
                        reason,
                        skip_conversation_restore: false,
                    });
                }
                EventReturn::SessionBeforeForkCancel {
                    reason,
                    skip_conversation_restore,
                } => {
                    return Ok(ForkDecision::Cancel {
                        reason,
                        skip_conversation_restore,
                    });
                }
                _ => {}
            }
        }
        Ok(ForkDecision::Proceed)
    }

    /// Check if session tree navigation should proceed. Returns
    /// `TreeDecision::Cancel` if any handler cancelled (via `Cancel` or
    /// `SessionBeforeTreeCancel`), otherwise `TreeDecision::Proceed`.
    pub async fn before_session_tree(
        &self,
        target_id: &str,
        old_leaf_id: Option<&str>,
    ) -> Result<TreeDecision> {
        let returns = self
            .dispatch(LifecycleEvent {
                kind: EventKind::SessionBeforeTree,
                payload: EventPayload::SessionBeforeTree {
                    target_id: target_id.to_owned(),
                    old_leaf_id: old_leaf_id.map(str::to_owned),
                },
            })
            .await?;

        for r in returns {
            match r {
                EventReturn::Cancel { reason } => {
                    return Ok(TreeDecision::Cancel {
                        reason,
                        summary: None,
                    });
                }
                EventReturn::SessionBeforeTreeCancel { reason, summary } => {
                    return Ok(TreeDecision::Cancel { reason, summary });
                }
                _ => {}
            }
        }
        Ok(TreeDecision::Proceed)
    }

    pub async fn before_compact(
        &self,
        custom_instructions: Option<&str>,
    ) -> Result<Option<String>> {
        let returns = self
            .dispatch(LifecycleEvent {
                kind: EventKind::SessionBeforeCompact,
                payload: EventPayload::SessionBeforeCompact {
                    custom_instructions: custom_instructions.map(str::to_owned),
                },
            })
            .await?;

        for r in &returns {
            if let EventReturn::Cancel { .. } = r {
                return Ok(None);
            }
        }
        for r in returns {
            if let EventReturn::CompactCustom {
                instructions,
                cancel,
                compaction: _,
            } = r
            {
                if cancel {
                    return Ok(None);
                }
                return Ok(Some(instructions));
            }
        }
        Ok(Some(custom_instructions.unwrap_or("").to_owned()))
    }

    // ── Message interception ────────────────────────────────────────────────────

    pub async fn on_message_end(
        &self,
        message: serde_json::Value,
    ) -> Result<Option<serde_json::Value>> {
        let returns = self
            .dispatch(LifecycleEvent {
                kind: EventKind::MessageEnd,
                payload: EventPayload::MessageEnd {
                    message: message.clone(),
                },
            })
            .await?;

        for r in returns {
            if let EventReturn::MessageReplace { message: replaced } = r {
                return Ok(Some(replaced));
            }
        }
        Ok(None)
    }

    // ── User bash ─────────────────────────────────────────────────────────────

    pub async fn on_user_bash(&self, command: &str, exclude: bool, cwd: &str) -> Result<bool> {
        self.dispatch_cancelable(LifecycleEvent {
            kind: EventKind::UserBash,
            payload: EventPayload::UserBash {
                command: command.to_owned(),
                exclude_from_context: exclude,
                cwd: cwd.to_owned(),
            },
        })
        .await
    }

    // ── Provider response ────────────────────────────────────────────────────

    pub async fn after_provider_response(
        &self,
        status: u16,
        headers: serde_json::Value,
    ) -> Result<()> {
        self.dispatch(LifecycleEvent {
            kind: EventKind::AfterProviderResponse,
            payload: EventPayload::AfterProviderResponse { status, headers },
        })
        .await?;
        Ok(())
    }

    // ── Message streaming ─────────────────────────────────────────────────────

    pub async fn on_message_start(&self, message: serde_json::Value) -> Result<()> {
        self.dispatch(LifecycleEvent {
            kind: EventKind::MessageStart,
            payload: EventPayload::MessageStart { message },
        })
        .await?;
        Ok(())
    }

    pub async fn on_message_update(
        &self,
        message: serde_json::Value,
        event: serde_json::Value,
    ) -> Result<()> {
        self.dispatch(LifecycleEvent {
            kind: EventKind::MessageUpdate,
            payload: EventPayload::MessageUpdate { message, event },
        })
        .await?;
        Ok(())
    }

    // ── Session compaction (post-compaction notification) ─────────────────────

    pub async fn on_session_compact(&self, from_extension: bool) -> Result<()> {
        self.dispatch(LifecycleEvent {
            kind: EventKind::SessionCompact,
            payload: EventPayload::SessionCompact { from_extension },
        })
        .await?;
        Ok(())
    }

    // ── Tool execution lifecycle ─────────────────────────────────────────────

    pub async fn on_tool_execution_start(
        &self,
        tool_call_id: &str,
        tool_name: &str,
        args: serde_json::Value,
    ) -> Result<()> {
        self.dispatch(LifecycleEvent {
            kind: EventKind::ToolExecutionStart,
            payload: EventPayload::ToolExecutionStart {
                tool_call_id: tool_call_id.to_owned(),
                tool_name: tool_name.to_owned(),
                args,
            },
        })
        .await?;
        Ok(())
    }

    pub async fn on_tool_execution_update(
        &self,
        tool_call_id: &str,
        tool_name: &str,
        partial_result: serde_json::Value,
    ) -> Result<()> {
        self.dispatch(LifecycleEvent {
            kind: EventKind::ToolExecutionUpdate,
            payload: EventPayload::ToolExecutionUpdate {
                tool_call_id: tool_call_id.to_owned(),
                tool_name: tool_name.to_owned(),
                partial_result,
            },
        })
        .await?;
        Ok(())
    }

    pub async fn on_tool_execution_end(
        &self,
        tool_call_id: &str,
        tool_name: &str,
        result: serde_json::Value,
        is_error: bool,
    ) -> Result<()> {
        self.dispatch(LifecycleEvent {
            kind: EventKind::ToolExecutionEnd,
            payload: EventPayload::ToolExecutionEnd {
                tool_call_id: tool_call_id.to_owned(),
                tool_name: tool_name.to_owned(),
                result,
                is_error,
            },
        })
        .await?;
        Ok(())
    }

    // ── Lifecycle helpers ─────────────────────────────────────────────────────

    pub async fn boot(&self) -> Result<()> {
        self.dispatch(LifecycleEvent::empty(EventKind::RuntimeStart))
            .await?;
        Ok(())
    }

    pub async fn start_session(&self) -> Result<()> {
        self.dispatch(LifecycleEvent {
            kind: EventKind::SessionStart,
            payload: EventPayload::SessionStart {
                cwd: self.cwd.display().to_string(),
                session_id: self.session_id.clone(),
                reason: Some("startup".to_owned()),
            },
        })
        .await?;
        Ok(())
    }

    pub async fn shutdown_session(&self) -> Result<()> {
        self.dispatch(LifecycleEvent {
            kind: EventKind::SessionShutdown,
            payload: EventPayload::SessionShutdown { reason: None },
        })
        .await?;
        Ok(())
    }

    pub async fn on_tui_ready(&self) -> Result<()> {
        self.dispatch(LifecycleEvent::empty(EventKind::TuiReady))
            .await?;
        Ok(())
    }

    pub async fn on_agent_start(&self) -> Result<()> {
        self.dispatch(LifecycleEvent::empty(EventKind::AgentStart))
            .await?;
        Ok(())
    }

    pub async fn on_agent_end(&self, stop_cause: &str, summary: Option<String>) -> Result<()> {
        self.dispatch(LifecycleEvent {
            kind: EventKind::AgentEnd,
            payload: EventPayload::AgentEnd {
                stop_cause: stop_cause.to_owned(),
                summary,
            },
        })
        .await?;
        Ok(())
    }

    pub async fn on_turn_start(&self, turn: u32) -> Result<()> {
        self.dispatch(LifecycleEvent {
            kind: EventKind::TurnStart,
            payload: EventPayload::TurnStart { turn },
        })
        .await?;
        Ok(())
    }

    pub async fn on_turn_end(&self, turn: u32, had_code: bool) -> Result<()> {
        self.dispatch(LifecycleEvent {
            kind: EventKind::TurnEnd,
            payload: EventPayload::TurnEnd { turn, had_code },
        })
        .await?;
        Ok(())
    }

    pub async fn on_tool_call(&self, name: &str, args: serde_json::Value) -> Result<()> {
        self.dispatch(LifecycleEvent {
            kind: EventKind::ToolCall,
            payload: EventPayload::ToolCall {
                name: name.to_owned(),
                args,
            },
        })
        .await?;
        Ok(())
    }

    /// Dispatch a `ToolCall` event and collect any interception returns.
    ///
    /// Returns `Some(ToolInterception)` if a handler blocked or overrode the
    /// tool arguments; `None` if the tool should proceed unmodified.
    pub async fn intercept_tool_call(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> Result<ToolInterception> {
        let returns = self
            .dispatch(LifecycleEvent {
                kind: EventKind::ToolCall,
                payload: EventPayload::ToolCall {
                    name: name.to_owned(),
                    args: args.clone(),
                },
            })
            .await?;

        for r in returns {
            match r {
                EventReturn::ToolBlock { reason } => {
                    return Ok(ToolInterception::Blocked { reason });
                }
                EventReturn::ToolArgsOverride {
                    args: override_args,
                } => {
                    return Ok(ToolInterception::ArgsOverridden {
                        args: override_args,
                    });
                }
                _ => {}
            }
        }
        Ok(ToolInterception::Proceed)
    }

    pub async fn on_tool_result(
        &self,
        name: &str,
        result: &str,
        error: Option<&str>,
    ) -> Result<()> {
        self.dispatch(LifecycleEvent {
            kind: EventKind::ToolResult,
            payload: EventPayload::ToolResult {
                name: name.to_owned(),
                result: result.to_owned(),
                error: error.map(str::to_owned),
            },
        })
        .await?;
        Ok(())
    }

    pub async fn on_model_select(
        &self,
        model: &str,
        previous: Option<&str>,
        source: &str,
    ) -> Result<()> {
        self.dispatch(LifecycleEvent {
            kind: EventKind::ModelSelect,
            payload: EventPayload::ModelSelect {
                model: model.to_owned(),
                previous_model: previous.map(str::to_owned),
                source: source.to_owned(),
            },
        })
        .await?;
        Ok(())
    }

    pub async fn on_thinking_level_select(&self, level: &str, previous: &str) -> Result<()> {
        self.dispatch(LifecycleEvent {
            kind: EventKind::ThinkingLevelSelect,
            payload: EventPayload::ThinkingLevelSelect {
                level: level.to_owned(),
                previous_level: previous.to_owned(),
            },
        })
        .await?;
        Ok(())
    }

    // ── Notifications ─────────────────────────────────────────────────────────

    pub fn push_notification(&self, msg: impl Into<String>) {
        self.notifications.lock().unwrap().push(msg.into());
    }

    pub fn drain_notifications(&self) -> Vec<String> {
        std::mem::take(&mut *self.notifications.lock().unwrap())
    }

    // ── Status ────────────────────────────────────────────────────────────────

    pub async fn set_status(&self, key: impl Into<String>, text: impl Into<String>) {
        self.status.lock().await.insert(key.into(), text.into());
    }

    pub async fn clear_status(&self, key: impl Into<String>) {
        self.status.lock().await.remove(&key.into());
    }

    pub async fn status_snapshot(&self) -> Vec<(String, String)> {
        self.status
            .lock()
            .await
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    // ── Session metadata ────────────────────────────────────────────────────────

    pub async fn set_session_name(&self, name: impl Into<String>) {
        *self.session_name.lock().await = Some(name.into());
    }

    pub async fn get_session_name(&self) -> Option<String> {
        self.session_name.lock().await.clone()
    }

    pub async fn set_label(&self, entry_id: impl Into<String>, label: Option<String>) {
        let id = entry_id.into();
        match label {
            Some(l) => {
                self.labels.lock().await.insert(id, l);
            }
            None => {
                self.labels.lock().await.remove(&id);
            }
        }
    }

    pub async fn labels_snapshot(&self) -> Vec<(String, String)> {
        self.labels
            .lock()
            .await
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    // ── Pending messages ────────────────────────────────────────────────────────

    pub fn push_pending_message(&self, msg: impl Into<String>) {
        self.pending_messages.lock().unwrap().push(msg.into());
    }

    pub fn drain_pending_messages(&self) -> Vec<String> {
        std::mem::take(&mut *self.pending_messages.lock().unwrap())
    }

    // ── Tool management ────────────────────────────────────────────────────────

    pub async fn get_active_tools(&self) -> Vec<String> {
        self.active_tools.lock().await.clone()
    }

    pub async fn set_active_tools(&self, tools: Vec<String>) {
        *self.active_tools.lock().await = tools;
    }

    pub fn all_tool_names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    // ── Flags ──────────────────────────────────────────────────────────────────

    pub fn get_flag(&self, name: &str) -> Option<&FlagSpec> {
        self.flags.get(name)
    }

    pub fn list_flags(&self) -> Vec<(String, String)> {
        self.flags
            .values()
            .map(|f| (f.name.clone(), f.description.clone()))
            .collect()
    }

    // ── Shortcuts ──────────────────────────────────────────────────────────────

    pub fn list_shortcuts(&self) -> Vec<(String, String)> {
        self.shortcuts
            .iter()
            .map(|s| (s.key.clone(), s.description.clone()))
            .collect()
    }

    // ── Providers ──────────────────────────────────────────────────────────────

    pub fn get_provider(&self, name: &str) -> Option<&ProviderConfig> {
        self.providers.get(name)
    }

    pub fn list_providers(&self) -> Vec<String> {
        self.providers.keys().cloned().collect()
    }

    // ── Message renderers ──────────────────────────────────────────────────────

    pub fn get_message_renderer(&self, custom_type: &str) -> Option<&MessageRendererSpec> {
        self.message_renderers.get(custom_type)
    }

    pub fn list_message_renderers(&self) -> Vec<String> {
        self.message_renderers.keys().cloned().collect()
    }

    // ── Tool render specs ──────────────────────────────────────────────────────

    /// Snapshot of the registered tool display metadata, seeded with the
    /// core `repl` spec and overlaid with the active style's specs (later
    /// inserts win in the index). Frontends take this at startup and
    /// re-take it after a style switch.
    pub fn tool_render_index(&self) -> ToolRenderIndex {
        let base = self.tool_renderers.values().cloned();
        match self.active_style_spec() {
            Some(style) => ToolRenderIndex::new(base.chain(style.tool_renderers.iter().cloned())),
            None => ToolRenderIndex::new(base),
        }
    }

    // ── Visual: style accessors (WS10) ──────────────────────────────────

    /// Get a registered style pack by name.
    pub fn get_style(&self, name: &str) -> Option<&StyleSpec> {
        self.styles.get(name)
    }

    /// List all registered style pack names.
    pub fn list_styles(&self) -> Vec<String> {
        self.styles.keys().cloned().collect()
    }

    /// The active style pack's name, if one is set.
    pub fn active_style(&self) -> Option<String> {
        self.active_style.lock().unwrap().clone()
    }

    /// Activate a style pack by name (`None` deactivates, falling back to
    /// the base registrations). Unknown names error and leave the current
    /// style untouched; hosts re-resolve their visuals after a successful
    /// switch.
    pub fn set_active_style(&self, name: Option<&str>) -> Result<()> {
        if let Some(name) = name
            && !self.styles.contains_key(name)
        {
            bail!(
                "style '{}' is not registered (available: {})",
                name,
                self.list_styles().join(", ")
            );
        }
        *self.active_style.lock().unwrap() = name.map(str::to_owned);
        Ok(())
    }

    /// The active style's spec, when an active style is set and registered.
    fn active_style_spec(&self) -> Option<&StyleSpec> {
        let name = self.active_style.lock().unwrap().clone()?;
        self.styles.get(&name)
    }

    /// The theme in effect: the active style's theme, else the first
    /// registered theme.
    pub fn active_theme(&self) -> Option<&ThemeSpec> {
        self.active_style_spec()
            .and_then(|s| s.theme.as_ref())
            .or_else(|| self.first_theme())
    }

    /// The spinner in effect: the active style's spinner, else the first
    /// registered spinner.
    pub fn active_spinner(&self) -> Option<&SpinnerSpec> {
        self.active_style_spec()
            .and_then(|s| s.spinner.as_ref())
            .or_else(|| self.first_spinner())
    }

    /// The thinking expressions in effect: the active style's, else the
    /// first registered set.
    pub fn active_thinking_expression(&self) -> Option<&ThinkingExpressionSpec> {
        self.active_style_spec()
            .and_then(|s| s.thinking_expression.as_ref())
            .or_else(|| self.first_thinking_expression())
    }

    /// The transcript tool chrome in effect: the active style's, else the
    /// stock Phi look. Chrome has no standalone registration — it is a
    /// singular surface, so "which chrome" is exactly "which style".
    pub fn active_chrome(&self) -> ChromeSpec {
        self.active_style_spec()
            .and_then(|s| s.chrome.clone())
            .unwrap_or_default()
    }

    // ── Agent profile accessors (WS1) ────────────────────────────────

    /// Get a registered agent profile by name.
    pub fn get_agent(&self, name: &str) -> Option<&crate::AgentSpec> {
        self.agents.get(name)
    }

    /// List all registered agent profile names.
    pub fn list_agents(&self) -> Vec<String> {
        self.agents.keys().cloned().collect()
    }

    /// The active agent profile's name, if one is set.
    pub fn active_agent(&self) -> Option<String> {
        self.active_agent.lock().unwrap().clone()
    }

    /// Activate an agent profile by name (`None` deactivates, falling back
    /// to the engine defaults). Unknown names error and leave the current
    /// profile untouched; the host re-resolves agent knobs after a successful
    /// switch.
    pub fn set_active_agent(&self, name: Option<&str>) -> Result<()> {
        if let Some(name) = name
            && !self.agents.contains_key(name)
        {
            bail!(
                "agent '{}' is not registered (available: {})",
                name,
                self.list_agents().join(", ")
            );
        }
        *self.active_agent.lock().unwrap() = name.map(str::to_owned);
        Ok(())
    }

    /// The active agent's spec, when an agent is set and registered.
    fn active_agent_spec(&self) -> Option<&crate::AgentSpec> {
        let name = self.active_agent.lock().unwrap().clone()?;
        self.agents.get(&name)
    }

    /// The system-prompt identity in effect: the active agent's
    /// `system_prompt`, if one is set and the agent carries one. `None` when
    /// no agent is active or the active agent's `system_prompt` is absent —
    /// the caller falls back to its own default identity (today's behavior,
    /// so bare core boots).
    pub fn active_agent_system_prompt(&self) -> Option<&str> {
        self.active_agent_spec()
            .and_then(|a| a.system_prompt.as_deref())
    }

    /// The orchestrator flag in effect: the active agent's `orchestrator`
    /// value, if one is set. `None` when no agent is active or the agent's
    /// `orchestrator` is absent — the caller falls back to its own default.
    pub fn active_agent_orchestrator(&self) -> Option<bool> {
        self.active_agent_spec().and_then(|a| a.orchestrator)
    }

    // ── Visual: theme accessors ──────────────────────────────────────────

    /// Get a registered theme by name.
    pub fn get_theme(&self, name: &str) -> Option<&ThemeSpec> {
        self.themes.get(name)
    }

    /// List all registered theme names.
    pub fn list_themes(&self) -> Vec<String> {
        self.themes.keys().cloned().collect()
    }

    /// Get the first registered theme (convenience for single-theme setups).
    pub fn first_theme(&self) -> Option<&ThemeSpec> {
        self.themes.values().next()
    }

    // ── Visual: spinner accessors ──────────────────────────────────────

    /// Get a registered spinner by name.
    pub fn get_spinner(&self, name: &str) -> Option<&SpinnerSpec> {
        self.spinners.get(name)
    }

    /// List all registered spinner names.
    pub fn list_spinners(&self) -> Vec<String> {
        self.spinners.keys().cloned().collect()
    }

    /// Get the first registered spinner.
    pub fn first_spinner(&self) -> Option<&SpinnerSpec> {
        self.spinners.values().next()
    }

    // ── Visual: thinking expression accessors ──────────────────────────

    /// Get registered thinking expressions by name.
    pub fn get_thinking_expression(&self, name: &str) -> Option<&ThinkingExpressionSpec> {
        self.thinking_expressions.get(name)
    }

    /// List all registered thinking expression names.
    pub fn list_thinking_expressions(&self) -> Vec<String> {
        self.thinking_expressions.keys().cloned().collect()
    }

    /// Get the first registered thinking expressions.
    pub fn first_thinking_expression(&self) -> Option<&ThinkingExpressionSpec> {
        self.thinking_expressions.values().next()
    }

    // ── Visual: text effect accessors ──────────────────────────────────

    /// Get a registered text effect by name.
    pub fn get_text_effect(&self, name: &str) -> Option<&TextEffectSpec> {
        self.text_effects.get(name)
    }

    /// List all registered text effect names.
    pub fn list_text_effects(&self) -> Vec<String> {
        self.text_effects.keys().cloned().collect()
    }

    /// Get the first registered text effect.
    pub fn first_text_effect(&self) -> Option<&TextEffectSpec> {
        self.text_effects.values().next()
    }

    // ── UI: overlay accessors ────────────────────────────────────────────

    /// Get a registered overlay by name.
    pub fn get_overlay(&self, name: &str) -> Option<&OverlaySpec> {
        self.overlays.get(name)
    }

    /// List all registered overlay names.
    pub fn list_overlays(&self) -> Vec<String> {
        self.overlays.keys().cloned().collect()
    }

    /// Get all registered overlays.
    pub fn all_overlays(&self) -> impl Iterator<Item = &OverlaySpec> {
        self.overlays.values()
    }

    // ── Commands ──────────────────────────────────────────────────────────────

    /// Look up a command in `context` by primary name, falling back to an
    /// exact alias match within the same context.
    fn command_in(&self, context: CommandContext, name: &str) -> Option<&CommandSpec> {
        if let Some(cmd) = self.commands.get(&(context, name.to_owned())) {
            return Some(cmd);
        }
        self.commands
            .values()
            .find(|c| c.context == context && c.aliases.iter().any(|a| a == name))
    }

    /// Session-context commands only: `values()` in a given context. Existing
    /// callers (session TUI) keep seeing only `Session`-context commands.
    fn session_commands(&self) -> impl Iterator<Item = &CommandSpec> {
        self.commands
            .values()
            .filter(|c| c.context == CommandContext::Session)
    }

    pub fn list_extension_commands(&self) -> Vec<(String, String)> {
        self.session_commands()
            .map(|c| (c.name.clone(), c.description.clone()))
            .collect()
    }

    /// Registry listing for completion popups and help output
    /// (Session-context commands only; aliases are not listed).
    pub fn list_commands(&self) -> Vec<crate::CommandInfo> {
        self.list_commands_in(CommandContext::Session)
    }

    /// Registry listing for one context (aliases are not listed).
    pub fn list_commands_in(&self, context: CommandContext) -> Vec<crate::CommandInfo> {
        self.commands
            .values()
            .filter(|c| c.context == context)
            .map(|c| crate::CommandInfo {
                name: c.name.clone(),
                args_hint: c.args_hint.clone(),
                description: c.description.clone(),
            })
            .collect()
    }

    /// First-argument completion candidates for a registered Session-context
    /// command, if the command declared a completion source.
    pub fn complete_command_arg(&self, name: &str) -> Option<Vec<String>> {
        self.command_in(CommandContext::Session, name)?
            .arg_completions
            .as_ref()
            .map(|f| f())
    }

    /// Invoke a command handler synchronously on the calling thread. The TUI
    /// uses this from its input path: handlers are expected to be quick (they
    /// return declarative `UiAction`s rather than doing UI work) and slow
    /// bridges (Lua) bound their own channel waits. Session context only.
    pub fn invoke_command_sync(&self, name: &str, raw_args: &str) -> Result<Option<CommandOutput>> {
        self.invoke_command_sync_in(CommandContext::Session, name, raw_args)
            .map_err(|e| anyhow!("command '{name}': {e}"))
    }

    /// Like [`Self::invoke_command_sync`] but for an explicit context;
    /// resolves aliases within that context. Handler errors are returned
    /// verbatim (no `command '{name}':` wrap) so hosts control presentation.
    pub fn invoke_command_sync_in(
        &self,
        context: CommandContext,
        name: &str,
        raw_args: &str,
    ) -> Result<Option<CommandOutput>> {
        let Some(cmd) = self.command_in(context, name) else {
            return Ok(None);
        };
        (cmd.handler)(CommandInvocation {
            raw_args: raw_args.to_owned(),
        })
        .map(Some)
    }

    pub async fn invoke_command(
        &self,
        name: &str,
        raw_args: &str,
    ) -> Result<Option<CommandOutput>> {
        let Some(cmd) = self.command_in(CommandContext::Session, name) else {
            return Ok(None);
        };
        let h = cmd.handler.clone();
        let args = raw_args.to_owned();
        // Bound the command handler the same way `dispatch` bounds event
        // handlers. Commands are user-initiated and may do real work, so allow
        // more time; on timeout surface a real error (not `Ok(None)`, which the
        // caller renders as "unknown command").
        let timeout = Duration::from_secs(10);
        match tokio::time::timeout(
            timeout,
            tokio::task::spawn_blocking(move || h(CommandInvocation { raw_args: args })),
        )
        .await
        {
            Ok(Ok(Ok(output))) => Ok(Some(output)),
            Ok(Ok(Err(e))) => Err(anyhow!("command '{name}': {e}")),
            Ok(Err(e)) => Err(anyhow!("command '{name}' join error: {e}")),
            Err(_elapsed) => Err(anyhow!("command '{name}' timed out after {timeout:?}")),
        }
    }

    // ── Tool invocation ───────────────────────────────────────────────────────

    pub fn invoke_tool_sync(&self, name: &str, input: &str) -> Result<Option<crate::ToolOutput>> {
        let Some(tool) = self.tools.get(name) else {
            return Ok(None);
        };
        let crate::ToolDispatch::Handler(h) = &tool.dispatch else {
            // Python-dispatch tools live in the REPL env; the engine calls
            // them there, never through the host handler path.
            return Err(anyhow!(
                "tool '{name}' is implemented in the REPL environment, not by a host handler"
            ));
        };
        let h = h.clone();
        let inv = crate::ToolInvocation {
            input: input.to_owned(),
        };
        Ok(Some(h(inv)?))
    }

    /// Invoke a handler-dispatched tool with the doctrine-2 watchdog: the
    /// handler runs on the blocking pool and the wait is bounded, so a hung
    /// extension tool cannot wedge the agent loop. `Ok(None)` means no such
    /// tool is registered.
    pub async fn invoke_tool(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> Result<Option<crate::ToolOutput>> {
        let Some(tool) = self.tools.get(name) else {
            return Ok(None);
        };
        let crate::ToolDispatch::Handler(h) = &tool.dispatch else {
            return Err(anyhow!(
                "tool '{name}' is implemented in the REPL environment, not by a host handler"
            ));
        };
        let h = h.clone();
        let input = serde_json::to_string(&args).unwrap_or_default();
        // Tools may do real work (network calls, subprocesses), so the budget
        // is generous — but it exists: a native tool call must always produce
        // a tool_result for the provider, even when the handler hangs.
        let timeout = Duration::from_secs(60);
        match tokio::time::timeout(
            timeout,
            tokio::task::spawn_blocking(move || h(crate::ToolInvocation { input })),
        )
        .await
        {
            Ok(Ok(Ok(output))) => Ok(Some(output)),
            Ok(Ok(Err(e))) => Err(anyhow!("tool '{name}': {e}")),
            Ok(Err(e)) => Err(anyhow!("tool '{name}' join error: {e}")),
            Err(_elapsed) => Err(anyhow!("tool '{name}' timed out after {timeout:?}")),
        }
    }

    // ── Tool prelude stubs ────────────────────────────────────────────────────

    /// Python stubs injected into the REPL for repl-exposed handler tools.
    /// Python-dispatch tools are skipped — their function already exists in
    /// the env (the prelude defines it); a stub would shadow it.
    pub fn extension_prelude_stubs(&self) -> String {
        self.tools
            .values()
            .filter(|t| {
                t.expose.includes_repl() && matches!(t.dispatch, crate::ToolDispatch::Handler(_))
            })
            .map(|t| {
                // Wire param names may be Python keywords or contain invalid
                // characters (MCP schemas are arbitrary); the signature uses
                // sanitized names while the dict keeps the wire names.
                let py_names = crate::python_param_names(&t.params);
                let sig: Vec<String> = t
                    .params
                    .iter()
                    .zip(&py_names)
                    .map(|(p, py)| match &p.default {
                        Some(def) => format!("{}: {} = {}", py, p.ty, def),
                        None => format!("{}: {}", py, p.ty),
                    })
                    .collect();
                let dict: Vec<String> = t
                    .params
                    .iter()
                    .zip(&py_names)
                    .map(|(p, py)| format!("\"{}\":  {}", p.name, py))
                    .collect();
                format!(
                    "def {}({}) -> str:\n    return __phi_ext_tool__(\"{}\" , {{{}}})",
                    t.name,
                    sig.join(", "),
                    t.name,
                    dict.join(", ")
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    pub fn tool_specs(&self) -> impl Iterator<Item = &ToolSpec> {
        self.tools.values()
    }

    /// Tools that surface as provider-level tool definitions (`expose:
    /// native | both`). The engine builds its per-turn tool list from this.
    pub fn native_tool_specs(&self) -> impl Iterator<Item = &ToolSpec> {
        self.tools.values().filter(|t| t.expose.includes_native())
    }

    /// Look up a native-exposed tool's dispatch kind. `None` when the name
    /// isn't registered or isn't native-exposed.
    pub fn native_tool_dispatch(&self, name: &str) -> Option<&crate::ToolDispatch> {
        self.tools
            .get(name)
            .filter(|t| t.expose.includes_native())
            .map(|t| &t.dispatch)
    }

    // ── Snapshot ──────────────────────────────────────────────────────────────

    pub fn snapshot(&self) -> RuntimeSnapshot {
        RuntimeSnapshot {
            cwd: self.cwd.display().to_string(),
            session_id: self.session_id.clone(),
            extensions: self.extensions.clone(),
            tools: self.tools.values().map(tool_summary).collect(),
            commands: self.commands.values().map(command_summary).collect(),
            shortcuts: self.shortcuts.iter().map(shortcut_summary).collect(),
            flags: self.flags.values().map(flag_summary).collect(),
            event_handlers: self
                .event_handlers
                .iter()
                .map(|handler| EventHandlerSummary {
                    event: handler.event,
                    label: handler.label.clone(),
                })
                .collect(),
            themes: self
                .themes
                .values()
                .map(|t| ThemeSummary {
                    name: t.name.clone(),
                    role_count: t.palette.len(),
                    has_resolver: t.resolver.is_some(),
                })
                .collect(),
            spinners: self.spinners.values().map(spinner_summary).collect(),
            thinking_expressions: self
                .thinking_expressions
                .values()
                .map(thinking_expression_summary)
                .collect(),
            text_effects: self
                .text_effects
                .values()
                .map(text_effect_summary)
                .collect(),
            overlays: self.overlays.values().map(overlay_summary).collect(),
        }
    }
}

// ── Result types ────────────────────────────────────────────────────────────

/// Result of project trust check.
pub struct ProjectTrustDecision {
    pub trusted: bool,
    pub remember: bool,
}

/// The outcome of intercepting a tool call via extensions.
#[derive(Clone, Debug)]
pub enum ToolInterception {
    /// No handler intercepted — proceed with the original call.
    Proceed,
    /// A handler blocked the tool; `reason` is surfaced to the model.
    Blocked { reason: String },
    /// A handler rewrote the tool arguments; use these instead.
    ArgsOverridden { args: serde_json::Value },
}

/// The outcome of intercepting a session fork via extensions.
#[derive(Clone, Debug)]
pub enum ForkDecision {
    /// No handler cancelled — proceed with the fork.
    Proceed,
    /// A handler cancelled the fork.
    Cancel {
        reason: String,
        skip_conversation_restore: bool,
    },
}

/// The outcome of intercepting a session tree navigation via extensions.
#[derive(Clone, Debug)]
pub enum TreeDecision {
    /// No handler cancelled — proceed with the tree navigation.
    Proceed,
    /// A handler cancelled the tree navigation.
    Cancel {
        reason: String,
        summary: Option<String>,
    },
}

/// Discovered resources from extension handlers.
#[derive(Default, Debug, Clone)]
pub struct DiscoveredResources {
    pub skill_paths: Vec<String>,
    pub prompt_paths: Vec<String>,
    pub theme_paths: Vec<String>,
}

pub fn missing_named_item(kind: &str, name: &str) -> anyhow::Error {
    anyhow!("{kind} '{name}' is not registered")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::{
        EventHandlerRegistration, EventKind, EventReturn, Extension, ExtensionHost,
        ExtensionManifest, LifecycleEvent,
    };

    use crate::RuntimeBuilder;

    /// Test extension that appends a fixed block to the system prompt on every
    /// `BeforeAgentStart` (mirrors `phi-memory`'s auto-inject handler).
    struct AppendingExt;

    impl Extension for AppendingExt {
        fn manifest(&self) -> ExtensionManifest {
            ExtensionManifest {
                id: "test.append".to_owned(),
                name: "Append".to_owned(),
                version: "0".to_owned(),
                description: "test hook".to_owned(),
            }
        }

        fn register(&self, host: &mut dyn ExtensionHost) -> anyhow::Result<()> {
            host.subscribe(EventHandlerRegistration {
                timeout: None,
                event: EventKind::BeforeAgentStart,
                label: "test.append".to_owned(),
                handler: Arc::new(|_ev: LifecycleEvent| {
                    Ok(Some(EventReturn::SystemPromptMod {
                        prepend: None,
                        append: Some("\n\nINJECTED-BY-HOOK".to_owned()),
                        replace: None,
                    }))
                }),
            })?;
            Ok(())
        }
    }

    /// Track 4 (plans/prompt-content-port.md): regression guard for the
    /// "unbounded growth" worry. The engine rebuilds the system message fresh
    /// each warm send (`initial_history`) and *then* applies
    /// `filter_system_prompt`, so a hook append never accumulates across turns.
    /// This test asserts the guard directly: repeated `filter_system_prompt`
    /// calls, each starting from a fresh-built system prompt, yield a constant
    /// length (the hook appends once per call, not once per session).
    #[tokio::test]
    async fn filter_system_prompt_does_not_grow_across_turns() {
        let rt = RuntimeBuilder::new()
            .with_cwd(std::path::Path::new("."))
            .register_extension(AppendingExt)
            .build()
            .expect("build runtime");
        let base = "BASE SYSTEM PROMPT".to_owned();

        let mut lengths = Vec::new();
        for _ in 0..5 {
            // Each iteration is a fresh rebuild (as initial_history does each
            // send), then the hook appends once.
            let filtered = rt
                .filter_system_prompt("user prompt", base.clone())
                .await
                .expect("filter");
            lengths.push(filtered.len());
        }
        assert!(
            lengths.windows(2).all(|w| w[0] == w[1]),
            "system prompt grew across turns: {lengths:?}"
        );

        // The hook must actually append, else the constant-length check is vacuous.
        let filtered = rt
            .filter_system_prompt("user prompt", base.clone())
            .await
            .expect("filter");
        assert!(filtered.contains("INJECTED-BY-HOOK"));
        assert!(filtered.len() > base.len());
    }

    #[tokio::test]
    async fn dispatch_cancelable_returns_true_on_cancel() {
        struct CancelExt;
        impl Extension for CancelExt {
            fn manifest(&self) -> ExtensionManifest {
                ExtensionManifest {
                    id: "test.cancel".to_owned(),
                    name: "Cancel".to_owned(),
                    version: "0".to_owned(),
                    description: "test cancel".to_owned(),
                }
            }
            fn register(&self, host: &mut dyn ExtensionHost) -> anyhow::Result<()> {
                host.subscribe(EventHandlerRegistration {
                    timeout: None,
                    event: EventKind::SessionBeforeSwitch,
                    label: "test.cancel".to_owned(),
                    handler: Arc::new(|_ev| {
                        Ok(Some(EventReturn::Cancel {
                            reason: "nope".to_owned(),
                        }))
                    }),
                })?;
                Ok(())
            }
        }

        let rt = RuntimeBuilder::new()
            .with_cwd(std::path::Path::new("."))
            .register_extension(CancelExt)
            .build()
            .expect("build runtime");

        let cancelled = rt
            .before_session_switch("new", None)
            .await
            .expect("dispatch");
        assert!(cancelled, "should be cancelled");
    }

    #[tokio::test]
    async fn discover_resources_aggregates_results() {
        struct ResourceExt;
        impl Extension for ResourceExt {
            fn manifest(&self) -> ExtensionManifest {
                ExtensionManifest {
                    id: "test.resources".to_owned(),
                    name: "Resources".to_owned(),
                    version: "0".to_owned(),
                    description: "test resources".to_owned(),
                }
            }
            fn register(&self, host: &mut dyn ExtensionHost) -> anyhow::Result<()> {
                host.subscribe(EventHandlerRegistration {
                    timeout: None,
                    event: EventKind::ResourcesDiscover,
                    label: "test.resources".to_owned(),
                    handler: Arc::new(|_ev| {
                        Ok(Some(EventReturn::ResourcesDiscovered {
                            skill_paths: vec!["/skills/foo".to_owned()],
                            prompt_paths: vec!["/prompts/bar".to_owned()],
                            theme_paths: vec![],
                        }))
                    }),
                })?;
                Ok(())
            }
        }

        let rt = RuntimeBuilder::new()
            .with_cwd(std::path::Path::new("."))
            .register_extension(ResourceExt)
            .build()
            .expect("build runtime");

        let resources = rt
            .discover_resources(".", "startup")
            .await
            .expect("discover");
        assert_eq!(resources.skill_paths, vec!["/skills/foo"]);
        assert_eq!(resources.prompt_paths, vec!["/prompts/bar"]);
        assert!(resources.theme_paths.is_empty());
    }

    #[tokio::test]
    async fn project_trust_defaults_to_trusted() {
        let rt = RuntimeBuilder::new()
            .with_cwd(std::path::Path::new("."))
            .build()
            .expect("build runtime");

        let decision = rt.check_project_trust(".").await.expect("trust check");
        assert!(decision.trusted);
        assert!(!decision.remember);
    }

    #[tokio::test]
    async fn session_metadata_round_trips() {
        let rt = RuntimeBuilder::new()
            .with_cwd(std::path::Path::new("."))
            .build()
            .expect("build runtime");

        assert_eq!(rt.get_session_name().await, None);
        rt.set_session_name("my-session").await;
        assert_eq!(rt.get_session_name().await, Some("my-session".to_owned()));

        rt.set_label("entry-1", Some("important".to_owned())).await;
        rt.set_label("entry-2", Some("bookmark".to_owned())).await;
        let labels = rt.labels_snapshot().await;
        assert_eq!(labels.len(), 2);
    }

    #[tokio::test]
    async fn active_tool_management_round_trips() {
        let rt = RuntimeBuilder::new()
            .with_cwd(std::path::Path::new("."))
            .build()
            .expect("build runtime");

        assert!(rt.get_active_tools().await.is_empty());
        rt.set_active_tools(vec!["bash".to_owned(), "read_file".to_owned()])
            .await;
        assert_eq!(rt.get_active_tools().await, vec!["bash", "read_file"]);
    }

    #[tokio::test]
    async fn pending_messages_round_trips() {
        let rt = RuntimeBuilder::new()
            .with_cwd(std::path::Path::new("."))
            .build()
            .expect("build runtime");

        rt.push_pending_message("hello");
        rt.push_pending_message("world");
        let msgs = rt.drain_pending_messages();
        assert_eq!(msgs, vec!["hello", "world"]);
        assert!(rt.drain_pending_messages().is_empty());
    }

    #[tokio::test]
    async fn before_compact_returns_instructions() {
        struct CompactExt;
        impl Extension for CompactExt {
            fn manifest(&self) -> ExtensionManifest {
                ExtensionManifest {
                    id: "test.compact".to_owned(),
                    name: "Compact".to_owned(),
                    version: "0".to_owned(),
                    description: "test compact".to_owned(),
                }
            }
            fn register(&self, host: &mut dyn ExtensionHost) -> anyhow::Result<()> {
                host.subscribe(EventHandlerRegistration {
                    timeout: None,
                    event: EventKind::SessionBeforeCompact,
                    label: "test.compact".to_owned(),
                    handler: Arc::new(|_ev| {
                        Ok(Some(EventReturn::CompactCustom {
                            instructions: "custom instructions".to_owned(),
                            cancel: false,
                            compaction: None,
                        }))
                    }),
                })?;
                Ok(())
            }
        }

        let rt = RuntimeBuilder::new()
            .with_cwd(std::path::Path::new("."))
            .register_extension(CompactExt)
            .build()
            .expect("build runtime");

        let instructions = rt.before_compact(None).await.expect("before compact");
        assert_eq!(instructions, Some("custom instructions".to_owned()));
    }

    #[tokio::test]
    async fn before_compact_cancelled_returns_none() {
        struct CancelCompactExt;
        impl Extension for CancelCompactExt {
            fn manifest(&self) -> ExtensionManifest {
                ExtensionManifest {
                    id: "test.cancel-compact".to_owned(),
                    name: "CancelCompact".to_owned(),
                    version: "0".to_owned(),
                    description: "test cancel compact".to_owned(),
                }
            }
            fn register(&self, host: &mut dyn ExtensionHost) -> anyhow::Result<()> {
                host.subscribe(EventHandlerRegistration {
                    timeout: None,
                    event: EventKind::SessionBeforeCompact,
                    label: "test.cancel-compact".to_owned(),
                    handler: Arc::new(|_ev| {
                        Ok(Some(EventReturn::Cancel {
                            reason: "no compaction".to_owned(),
                        }))
                    }),
                })?;
                Ok(())
            }
        }

        let rt = RuntimeBuilder::new()
            .with_cwd(std::path::Path::new("."))
            .register_extension(CancelCompactExt)
            .build()
            .expect("build runtime");

        let instructions = rt.before_compact(None).await.expect("before compact");
        assert!(instructions.is_none());
    }

    // ── Tool exposure / dispatch (WS9.3a) ────────────────────────────────────

    /// Registers one tool per (exposure, dispatch) shape the API supports.
    struct ExposedToolsExt;
    impl Extension for ExposedToolsExt {
        fn manifest(&self) -> ExtensionManifest {
            ExtensionManifest {
                id: "test.tools".to_owned(),
                name: "Tools".to_owned(),
                version: "0".to_owned(),
                description: "test tools".to_owned(),
            }
        }
        fn register(&self, host: &mut dyn ExtensionHost) -> anyhow::Result<()> {
            host.register_tool(crate::ToolSpec::repl(
                "repl_tool",
                "repl-only handler tool",
                Vec::new(),
                Arc::new(|_inv| {
                    Ok(crate::ToolOutput {
                        content: "repl".to_owned(),
                    })
                }),
            ))?;
            host.register_tool(crate::ToolSpec {
                name: "native_tool".to_owned(),
                description: "native handler tool".to_owned(),
                params: Vec::new(),
                expose: crate::ToolExposure::Native,
                input_schema: None,
                dispatch: crate::ToolDispatch::Handler(Arc::new(|inv| {
                    Ok(crate::ToolOutput {
                        content: format!("handled:{}", inv.input),
                    })
                })),
            })?;
            host.register_tool(crate::ToolSpec {
                name: "py_tool".to_owned(),
                description: "python-implemented tool, both surfaces".to_owned(),
                params: Vec::new(),
                expose: crate::ToolExposure::Both,
                input_schema: None,
                dispatch: crate::ToolDispatch::Python,
            })?;
            Ok(())
        }
    }

    fn tools_runtime() -> crate::AppRuntime {
        RuntimeBuilder::new()
            .with_cwd(std::path::Path::new("."))
            .register_extension(ExposedToolsExt)
            .build()
            .expect("build runtime")
    }

    #[test]
    fn exposure_filters_prelude_stubs_and_native_list() {
        let rt = tools_runtime();

        // Stubs: only repl-exposed handler tools. Python-dispatch tools are
        // already defined in the env; native-only tools never surface there.
        let stubs = rt.extension_prelude_stubs();
        assert!(stubs.contains("def repl_tool("), "{stubs}");
        assert!(!stubs.contains("native_tool"), "{stubs}");
        assert!(!stubs.contains("py_tool"), "{stubs}");

        // Native list: native + both.
        let native: Vec<&str> = rt.native_tool_specs().map(|t| t.name.as_str()).collect();
        assert_eq!(native, vec!["native_tool", "py_tool"]);

        // Dispatch lookup respects exposure.
        assert!(matches!(
            rt.native_tool_dispatch("native_tool"),
            Some(crate::ToolDispatch::Handler(_))
        ));
        assert!(matches!(
            rt.native_tool_dispatch("py_tool"),
            Some(crate::ToolDispatch::Python)
        ));
        assert!(rt.native_tool_dispatch("repl_tool").is_none());
        assert!(rt.native_tool_dispatch("missing").is_none());
    }

    #[test]
    fn stub_signature_sanitizes_keyword_params_but_keeps_wire_names() {
        struct KeywordParamExt;
        impl Extension for KeywordParamExt {
            fn manifest(&self) -> ExtensionManifest {
                ExtensionManifest {
                    id: "test.kw".to_owned(),
                    name: "Kw".to_owned(),
                    version: "0".to_owned(),
                    description: "keyword param tool".to_owned(),
                }
            }
            fn register(&self, host: &mut dyn ExtensionHost) -> anyhow::Result<()> {
                host.register_tool(crate::ToolSpec::repl(
                    "mail_search",
                    "search",
                    vec![
                        crate::ToolParam {
                            name: "from".to_owned(),
                            ty: "str".to_owned(),
                            default: None,
                        },
                        crate::ToolParam {
                            name: "max-results".to_owned(),
                            ty: "int".to_owned(),
                            default: Some("None".to_owned()),
                        },
                    ],
                    Arc::new(|_inv| {
                        Ok(crate::ToolOutput {
                            content: String::new(),
                        })
                    }),
                ))
            }
        }

        let rt = RuntimeBuilder::new()
            .with_cwd(std::path::Path::new("."))
            .register_extension(KeywordParamExt)
            .build()
            .expect("build runtime");
        let stubs = rt.extension_prelude_stubs();
        // Signature: valid Python (keyword suffixed, dash replaced)...
        assert!(
            stubs.contains("def mail_search(from_: str, max_results: int = None)"),
            "{stubs}"
        );
        // ...while the dict keys keep the wire names the handler expects.
        assert!(stubs.contains("\"from\":  from_"), "{stubs}");
        assert!(stubs.contains("\"max-results\":  max_results"), "{stubs}");
    }

    #[tokio::test]
    async fn invoke_tool_runs_handler_and_rejects_python_dispatch() {
        let rt = tools_runtime();

        let out = rt
            .invoke_tool("native_tool", serde_json::json!({"x": 1}))
            .await
            .expect("invoke")
            .expect("registered");
        assert_eq!(out.content, "handled:{\"x\":1}");

        // Python-dispatch tools are executed in the REPL env by the engine,
        // never through the handler path.
        assert!(
            rt.invoke_tool("py_tool", serde_json::json!({}))
                .await
                .is_err()
        );
        // Unknown tool: None, not an error.
        assert!(
            rt.invoke_tool("missing", serde_json::json!({}))
                .await
                .expect("invoke")
                .is_none()
        );
    }

    // ── dispatch timeout / continue-on-error ─────────────────────────────────

    #[tokio::test]
    async fn dispatch_times_out_blocked_handler() {
        use std::time::{Duration, Instant};

        // A handler that blocks in a native call past the per-kind budget
        // (AgentStart is a 250ms notification). Without the central timeout
        // this would wedge the test for 2s; with it, dispatch returns ~250ms.
        struct BlockingExt;
        impl Extension for BlockingExt {
            fn manifest(&self) -> ExtensionManifest {
                ExtensionManifest {
                    id: "test.block".to_owned(),
                    name: "Block".to_owned(),
                    version: "0".to_owned(),
                    description: "test block".to_owned(),
                }
            }
            fn register(&self, host: &mut dyn ExtensionHost) -> anyhow::Result<()> {
                host.subscribe(EventHandlerRegistration {
                    timeout: None,
                    event: EventKind::AgentStart,
                    label: "test.block".to_owned(),
                    handler: Arc::new(|_ev| {
                        std::thread::sleep(Duration::from_secs(2));
                        Ok(None)
                    }),
                })?;
                Ok(())
            }
        }

        let rt = RuntimeBuilder::new()
            .with_cwd(std::path::Path::new("."))
            .register_extension(BlockingExt)
            .build()
            .expect("build runtime");

        let start = Instant::now();
        let returns = rt
            .dispatch(LifecycleEvent::empty(EventKind::AgentStart))
            .await
            .expect("dispatch");
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(1),
            "dispatch took {elapsed:?}; expected the ~250ms budget, not the 2s block"
        );
        assert!(
            returns.is_empty(),
            "a timed-out handler must contribute no return"
        );
        let notes = rt.drain_notifications().join("\n");
        assert!(
            notes.contains("timed out"),
            "missing timeout notification: {notes}"
        );
    }

    #[tokio::test]
    async fn dispatch_continues_past_erroring_handler() {
        // Two handlers on the same event: the first errors, the second returns
        // a value. Continue-on-error means the second handler still runs and its
        // return is collected (the old `??` would have aborted after the first).
        struct MultiExt;
        impl Extension for MultiExt {
            fn manifest(&self) -> ExtensionManifest {
                ExtensionManifest {
                    id: "test.multi".to_owned(),
                    name: "Multi".to_owned(),
                    version: "0".to_owned(),
                    description: "test multi".to_owned(),
                }
            }
            fn register(&self, host: &mut dyn ExtensionHost) -> anyhow::Result<()> {
                host.subscribe(EventHandlerRegistration {
                    timeout: None,
                    event: EventKind::ModelSelect,
                    label: "test.multi.err".to_owned(),
                    handler: Arc::new(|_ev| Err(anyhow::anyhow!("boom"))),
                })?;
                host.subscribe(EventHandlerRegistration {
                    timeout: None,
                    event: EventKind::ModelSelect,
                    label: "test.multi.ok".to_owned(),
                    handler: Arc::new(|_ev| {
                        Ok(Some(EventReturn::SystemPromptMod {
                            prepend: None,
                            append: Some("ok".to_owned()),
                            replace: None,
                        }))
                    }),
                })?;
                Ok(())
            }
        }

        let rt = RuntimeBuilder::new()
            .with_cwd(std::path::Path::new("."))
            .register_extension(MultiExt)
            .build()
            .expect("build runtime");

        let returns = rt
            .dispatch(LifecycleEvent::empty(EventKind::ModelSelect))
            .await
            .expect("dispatch");
        assert_eq!(
            returns.len(),
            1,
            "continue-on-error lost the second handler's return: {returns:?}"
        );
        let notes = rt.drain_notifications().join("\n");
        assert!(
            notes.contains("errored"),
            "missing error notification: {notes}"
        );
    }

    // ── Extended return surface tests ───────────────────────────────────────

    /// `SystemPromptMod::replace` substitutes the entire system prompt.
    #[tokio::test]
    async fn system_prompt_replace_substitutes_entire_prompt() {
        struct ReplaceExt;
        impl Extension for ReplaceExt {
            fn manifest(&self) -> ExtensionManifest {
                ExtensionManifest {
                    id: "test.replace".to_owned(),
                    name: "Replace".to_owned(),
                    version: "0".to_owned(),
                    description: "test replace".to_owned(),
                }
            }
            fn register(&self, host: &mut dyn ExtensionHost) -> anyhow::Result<()> {
                host.subscribe(EventHandlerRegistration {
                    timeout: None,
                    event: EventKind::BeforeAgentStart,
                    label: "test.replace".to_owned(),
                    handler: Arc::new(|_ev| {
                        Ok(Some(EventReturn::SystemPromptMod {
                            prepend: None,
                            append: None,
                            replace: Some("REPLACED PROMPT".to_owned()),
                        }))
                    }),
                })?;
                Ok(())
            }
        }

        let rt = RuntimeBuilder::new()
            .with_cwd(std::path::Path::new("."))
            .register_extension(ReplaceExt)
            .build()
            .expect("build runtime");

        let result = rt
            .filter_system_prompt("user", "ORIGINAL SYSTEM PROMPT".to_owned())
            .await
            .expect("filter");
        assert_eq!(result, "REPLACED PROMPT");
    }

    /// `ToolArgsOverride` is returned by `intercept_tool_call`.
    #[tokio::test]
    async fn intercept_tool_call_returns_args_override() {
        struct ArgsOverrideExt;
        impl Extension for ArgsOverrideExt {
            fn manifest(&self) -> ExtensionManifest {
                ExtensionManifest {
                    id: "test.args_override".to_owned(),
                    name: "ArgsOverride".to_owned(),
                    version: "0".to_owned(),
                    description: "test args override".to_owned(),
                }
            }
            fn register(&self, host: &mut dyn ExtensionHost) -> anyhow::Result<()> {
                host.subscribe(EventHandlerRegistration {
                    timeout: None,
                    event: EventKind::ToolCall,
                    label: "test.args_override".to_owned(),
                    handler: Arc::new(|_ev| {
                        Ok(Some(EventReturn::ToolArgsOverride {
                            args: serde_json::json!({"code": "print('overridden')"}),
                        }))
                    }),
                })?;
                Ok(())
            }
        }

        let rt = RuntimeBuilder::new()
            .with_cwd(std::path::Path::new("."))
            .register_extension(ArgsOverrideExt)
            .build()
            .expect("build runtime");

        let interception = rt
            .intercept_tool_call("repl", serde_json::json!({"code": "print('original')"}))
            .await
            .expect("intercept");

        match interception {
            crate::ToolInterception::ArgsOverridden { args } => {
                assert_eq!(args["code"], "print('overridden')");
            }
            other => panic!("expected ArgsOverridden, got {other:?}"),
        }
    }

    /// `CompactCustom` with `cancel: true` aborts compaction.
    #[tokio::test]
    async fn compact_cancel_returns_none() {
        struct CancelCompactExt;
        impl Extension for CancelCompactExt {
            fn manifest(&self) -> ExtensionManifest {
                ExtensionManifest {
                    id: "test.cancel_compact".to_owned(),
                    name: "CancelCompact".to_owned(),
                    version: "0".to_owned(),
                    description: "test cancel compact".to_owned(),
                }
            }
            fn register(&self, host: &mut dyn ExtensionHost) -> anyhow::Result<()> {
                host.subscribe(EventHandlerRegistration {
                    timeout: None,
                    event: EventKind::SessionBeforeCompact,
                    label: "test.cancel_compact".to_owned(),
                    handler: Arc::new(|_ev| {
                        Ok(Some(EventReturn::CompactCustom {
                            instructions: String::new(),
                            cancel: true,
                            compaction: None,
                        }))
                    }),
                })?;
                Ok(())
            }
        }

        let rt = RuntimeBuilder::new()
            .with_cwd(std::path::Path::new("."))
            .register_extension(CancelCompactExt)
            .build()
            .expect("build runtime");

        let result = rt.before_compact(Some("default")).await.expect("compact");
        assert!(result.is_none(), "compaction should be cancelled");
    }

    /// `before_session_fork` returns `ForkDecision::Cancel` with
    /// `skip_conversation_restore`.
    #[tokio::test]
    async fn fork_cancel_with_skip_conversation_restore() {
        struct ForkCancelExt;
        impl Extension for ForkCancelExt {
            fn manifest(&self) -> ExtensionManifest {
                ExtensionManifest {
                    id: "test.fork_cancel".to_owned(),
                    name: "ForkCancel".to_owned(),
                    version: "0".to_owned(),
                    description: "test fork cancel".to_owned(),
                }
            }
            fn register(&self, host: &mut dyn ExtensionHost) -> anyhow::Result<()> {
                host.subscribe(EventHandlerRegistration {
                    timeout: None,
                    event: EventKind::SessionBeforeFork,
                    label: "test.fork_cancel".to_owned(),
                    handler: Arc::new(|_ev| {
                        Ok(Some(EventReturn::SessionBeforeForkCancel {
                            reason: "no fork".to_owned(),
                            skip_conversation_restore: true,
                        }))
                    }),
                })?;
                Ok(())
            }
        }

        let rt = RuntimeBuilder::new()
            .with_cwd(std::path::Path::new("."))
            .register_extension(ForkCancelExt)
            .build()
            .expect("build runtime");

        let decision = rt
            .before_session_fork("entry-1", "after")
            .await
            .expect("fork");

        match decision {
            crate::ForkDecision::Cancel {
                reason,
                skip_conversation_restore,
            } => {
                assert_eq!(reason, "no fork");
                assert!(skip_conversation_restore);
            }
            other => panic!("expected Cancel, got {other:?}"),
        }
    }

    /// `before_session_tree` returns `TreeDecision::Cancel` with summary.
    #[tokio::test]
    async fn tree_cancel_with_summary() {
        struct TreeCancelExt;
        impl Extension for TreeCancelExt {
            fn manifest(&self) -> ExtensionManifest {
                ExtensionManifest {
                    id: "test.tree_cancel".to_owned(),
                    name: "TreeCancel".to_owned(),
                    version: "0".to_owned(),
                    description: "test tree cancel".to_owned(),
                }
            }
            fn register(&self, host: &mut dyn ExtensionHost) -> anyhow::Result<()> {
                host.subscribe(EventHandlerRegistration {
                    timeout: None,
                    event: EventKind::SessionBeforeTree,
                    label: "test.tree_cancel".to_owned(),
                    handler: Arc::new(|_ev| {
                        Ok(Some(EventReturn::SessionBeforeTreeCancel {
                            reason: "no tree".to_owned(),
                            summary: Some("custom summary".to_owned()),
                        }))
                    }),
                })?;
                Ok(())
            }
        }

        let rt = RuntimeBuilder::new()
            .with_cwd(std::path::Path::new("."))
            .register_extension(TreeCancelExt)
            .build()
            .expect("build runtime");

        let decision = rt
            .before_session_tree("target-1", None)
            .await
            .expect("tree");

        match decision {
            crate::TreeDecision::Cancel { reason, summary } => {
                assert_eq!(reason, "no tree");
                assert_eq!(summary.as_deref(), Some("custom summary"));
            }
            other => panic!("expected Cancel, got {other:?}"),
        }
    }

    /// A registration's `timeout` overrides the per-kind dispatch budget in
    /// both directions: a lowered budget drops a slow handler's return, a
    /// raised budget keeps a handler alive past where the (larger) kind
    /// default would have been irrelevant anyway — asserted here by pairing
    /// two identical slow handlers whose only difference is the override.
    #[tokio::test]
    async fn per_registration_timeout_overrides_kind_default() {
        use std::time::Duration;

        struct SlowGateExt;
        impl Extension for SlowGateExt {
            fn manifest(&self) -> ExtensionManifest {
                ExtensionManifest {
                    id: "test.slow_gate".to_owned(),
                    name: "SlowGate".to_owned(),
                    version: "0".to_owned(),
                    description: "test timeout override".to_owned(),
                }
            }
            fn register(&self, host: &mut dyn ExtensionHost) -> anyhow::Result<()> {
                let slow_handler = || {
                    Arc::new(|_ev: LifecycleEvent| {
                        std::thread::sleep(Duration::from_millis(300));
                        Ok(Some(EventReturn::ToolBlock {
                            reason: "gated".to_owned(),
                        }))
                    })
                };
                // Tight override: dropped despite the 2s ToolCall default.
                host.subscribe(EventHandlerRegistration {
                    event: EventKind::ToolCall,
                    label: "test.slow_gate.tight".to_owned(),
                    handler: slow_handler(),
                    timeout: Some(Duration::from_millis(50)),
                })?;
                // Generous override: collected.
                host.subscribe(EventHandlerRegistration {
                    event: EventKind::ToolCall,
                    label: "test.slow_gate.roomy".to_owned(),
                    handler: slow_handler(),
                    timeout: Some(Duration::from_secs(5)),
                })?;
                Ok(())
            }
        }

        let rt = RuntimeBuilder::new()
            .with_cwd(std::path::Path::new("."))
            .register_extension(SlowGateExt)
            .build()
            .expect("build runtime");

        let returns = rt
            .dispatch(LifecycleEvent {
                kind: EventKind::ToolCall,
                payload: crate::EventPayload::ToolCall {
                    name: "bash".to_owned(),
                    args: serde_json::json!({}),
                },
            })
            .await
            .expect("dispatch");

        assert_eq!(
            returns.len(),
            1,
            "only the roomy registration's return survives"
        );
        let notes = rt.drain_notifications();
        assert!(
            notes
                .iter()
                .any(|n| n.contains("test.slow_gate.tight") && n.contains("timed out")),
            "tight registration times out: {notes:?}"
        );
    }

    /// `ExtensionHost::ui_handle` hands out the same late-bound handle the
    /// built runtime exposes: a host connecting via `AppRuntime::connect_ui`
    /// is reachable from a handler that captured the handle at register time.
    #[tokio::test]
    async fn ui_handle_connects_extensions_to_late_host() {
        use std::time::Duration;

        struct AskingExt;
        impl Extension for AskingExt {
            fn manifest(&self) -> ExtensionManifest {
                ExtensionManifest {
                    id: "test.asking".to_owned(),
                    name: "Asking".to_owned(),
                    version: "0".to_owned(),
                    description: "test ui handle".to_owned(),
                }
            }
            fn register(&self, host: &mut dyn ExtensionHost) -> anyhow::Result<()> {
                let ui = host.ui_handle();
                assert!(!ui.has_ui(), "disconnected at register time");
                host.subscribe(EventHandlerRegistration {
                    event: EventKind::ToolCall,
                    label: "test.asking".to_owned(),
                    handler: Arc::new(move |_ev| {
                        let outcome = ui.ask(
                            crate::UiPrompt {
                                title: "Allow?".to_owned(),
                                body: String::new(),
                                options: vec![
                                    crate::UiPromptOption::new("Allow"),
                                    crate::UiPromptOption::new("Deny"),
                                ],
                            },
                            Duration::from_secs(5),
                        );
                        match outcome {
                            crate::UiAskOutcome::Answer(0) => Ok(None),
                            _ => Ok(Some(EventReturn::ToolBlock {
                                reason: "denied".to_owned(),
                            })),
                        }
                    }),
                    timeout: Some(Duration::from_secs(6)),
                })?;
                Ok(())
            }
        }

        let rt = RuntimeBuilder::new()
            .with_cwd(std::path::Path::new("."))
            .register_extension(AskingExt)
            .build()
            .expect("build runtime");

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        rt.connect_ui(tx);
        assert!(rt.ui_handle().has_ui());

        // Host side: answer "Allow" (index 0) to the one prompt.
        let host = tokio::spawn(async move {
            let req = rx.recv().await.expect("prompt arrives");
            assert_eq!(req.prompt.title, "Allow?");
            req.reply.send(Some(0)).expect("buffered reply");
        });

        let returns = rt
            .dispatch(LifecycleEvent {
                kind: EventKind::ToolCall,
                payload: crate::EventPayload::ToolCall {
                    name: "bash".to_owned(),
                    args: serde_json::json!({}),
                },
            })
            .await
            .expect("dispatch");
        host.await.unwrap();
        assert!(returns.is_empty(), "allowed call yields no ToolBlock");
    }

    // ── Styles (WS10) ───────────────────────────────────────────────────

    /// An extension registering a base theme/spinner plus one style pack
    /// bundling its own theme, spinner, and a tool renderer override.
    struct StyledExt;
    impl Extension for StyledExt {
        fn manifest(&self) -> ExtensionManifest {
            ExtensionManifest {
                id: "test.styled".to_owned(),
                name: "Styled".to_owned(),
                version: "0".to_owned(),
                description: "style pack test".to_owned(),
            }
        }
        fn register(&self, host: &mut dyn ExtensionHost) -> anyhow::Result<()> {
            let mut base_palette = std::collections::BTreeMap::new();
            base_palette.insert("accent".to_owned(), crate::Rgb(1, 2, 3));
            host.register_theme(crate::ThemeSpec::new("base", base_palette))?;
            host.register_spinner(crate::SpinnerSpec::new(
                "base",
                vec!["-".to_owned(), "|".to_owned()],
                80,
            ))?;
            host.register_tool_renderer(crate::ToolRenderSpec::new("bash").with_verb("run"))?;

            let mut pack_palette = std::collections::BTreeMap::new();
            pack_palette.insert("accent".to_owned(), crate::Rgb(9, 8, 7));
            host.register_style(
                crate::StyleSpec::new("crush")
                    .with_theme(crate::ThemeSpec::new("crush:theme", pack_palette))
                    .with_spinner(crate::SpinnerSpec::new(
                        "crush:spinner",
                        vec![".".to_owned()],
                        120,
                    ))
                    .with_tool_renderers(vec![crate::ToolRenderSpec::new("bash").with_verb("exec")])
                    .with_chrome(crate::ChromeSpec {
                        anchor_format: "{verb}({args})".to_owned(),
                        container: crate::ChromeContainer::Flat,
                        ..Default::default()
                    }),
            )?;
            // A minimal pack with no members: falls back to the base
            // registrations entirely (the shipped `phi` default's shape).
            host.register_style(crate::StyleSpec::new("bare"))?;
            Ok(())
        }
    }

    #[test]
    fn style_activation_overlays_and_falls_back() {
        let rt = RuntimeBuilder::new()
            .with_cwd(std::path::Path::new("."))
            .register_extension(StyledExt)
            .build()
            .unwrap();

        assert_eq!(rt.list_styles(), vec!["bare", "crush"]);
        assert_eq!(rt.active_style(), None);

        // No active style: active_* fall through to the base registrations.
        assert_eq!(rt.active_theme().unwrap().name, "base");
        assert_eq!(rt.active_spinner().unwrap().name, "base");
        assert_eq!(rt.active_chrome(), crate::ChromeSpec::default());
        assert_eq!(
            rt.tool_render_index().get("bash").unwrap().verb.as_deref(),
            Some("run")
        );

        // Active style: bundled members win, tool renderers overlay.
        rt.set_active_style(Some("crush")).unwrap();
        assert_eq!(rt.active_style().as_deref(), Some("crush"));
        assert_eq!(rt.active_theme().unwrap().name, "crush:theme");
        assert_eq!(rt.active_spinner().unwrap().name, "crush:spinner");
        assert_eq!(
            rt.tool_render_index().get("bash").unwrap().verb.as_deref(),
            Some("exec")
        );
        // Thinking expressions: the pack has none, so base fallback (none
        // registered here) applies.
        assert!(rt.active_thinking_expression().is_none());
        // Chrome: the pack's bundle applies while active.
        let chrome = rt.active_chrome();
        assert_eq!(chrome.format_anchor("Read", Some("a.rs")), "Read(a.rs)");
        assert!(chrome.is_flat());

        // An empty bundle behaves exactly like no style at all.
        rt.set_active_style(Some("bare")).unwrap();
        assert_eq!(rt.active_chrome(), crate::ChromeSpec::default());
        assert_eq!(rt.active_theme().unwrap().name, "base");
        assert_eq!(
            rt.tool_render_index().get("bash").unwrap().verb.as_deref(),
            Some("run")
        );

        // Deactivation restores the base view.
        rt.set_active_style(None).unwrap();
        assert_eq!(rt.active_style(), None);
        assert_eq!(rt.active_theme().unwrap().name, "base");
    }

    #[test]
    fn unknown_style_errors_and_preserves_active() {
        let rt = RuntimeBuilder::new()
            .with_cwd(std::path::Path::new("."))
            .register_extension(StyledExt)
            .build()
            .unwrap();
        rt.set_active_style(Some("crush")).unwrap();
        let err = rt.set_active_style(Some("nope")).unwrap_err();
        assert!(err.to_string().contains("not registered"), "{err}");
        assert!(err.to_string().contains("crush"), "lists available: {err}");
        assert_eq!(rt.active_style().as_deref(), Some("crush"));
    }

    #[test]
    fn duplicate_style_registration_is_rejected() {
        struct DupExt;
        impl Extension for DupExt {
            fn manifest(&self) -> ExtensionManifest {
                ExtensionManifest {
                    id: "test.dup".to_owned(),
                    name: "Dup".to_owned(),
                    version: "0".to_owned(),
                    description: String::new(),
                }
            }
            fn register(&self, host: &mut dyn ExtensionHost) -> anyhow::Result<()> {
                host.register_style(crate::StyleSpec::new("same"))?;
                host.register_style(crate::StyleSpec::new("same"))?;
                Ok(())
            }
        }
        let err = match RuntimeBuilder::new()
            .with_cwd(std::path::Path::new("."))
            .register_extension(DupExt)
            .build()
        {
            Ok(_) => panic!("duplicate style registration must fail the build"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("already registered"), "{err}");
    }

    #[test]
    fn agent_activation_overlays_and_falls_back() {
        struct AgentExt;
        impl Extension for AgentExt {
            fn manifest(&self) -> ExtensionManifest {
                ExtensionManifest {
                    id: "test.agent".to_owned(),
                    name: "Agent".to_owned(),
                    version: "0".to_owned(),
                    description: String::new(),
                }
            }
            fn register(&self, host: &mut dyn ExtensionHost) -> anyhow::Result<()> {
                host.register_agent(
                    crate::AgentSpec::new("rlm").with_system_prompt("You are an RLM."),
                )?;
                host.register_agent(crate::AgentSpec::new("bare"))?;
                Ok(())
            }
        }
        let rt = RuntimeBuilder::new()
            .with_cwd(std::path::Path::new("."))
            .register_extension(AgentExt)
            .build()
            .unwrap();
        assert_eq!(rt.list_agents(), vec!["bare", "rlm"]);
        assert_eq!(rt.active_agent(), None);
        // No agent active: system_prompt falls through (None).
        assert_eq!(rt.active_agent_system_prompt(), None);

        rt.set_active_agent(Some("rlm")).unwrap();
        assert_eq!(rt.active_agent().as_deref(), Some("rlm"));
        assert_eq!(rt.active_agent_system_prompt(), Some("You are an RLM."));

        // Deactivate: falls back to None.
        rt.set_active_agent(None).unwrap();
        assert_eq!(rt.active_agent(), None);
        assert_eq!(rt.active_agent_system_prompt(), None);
    }

    #[test]
    fn unknown_agent_errors_and_preserves_active() {
        struct AgentExt;
        impl Extension for AgentExt {
            fn manifest(&self) -> ExtensionManifest {
                ExtensionManifest {
                    id: "test.agent2".to_owned(),
                    name: "Agent2".to_owned(),
                    version: "0".to_owned(),
                    description: String::new(),
                }
            }
            fn register(&self, host: &mut dyn ExtensionHost) -> anyhow::Result<()> {
                host.register_agent(crate::AgentSpec::new("rlm"))?;
                Ok(())
            }
        }
        let rt = RuntimeBuilder::new()
            .with_cwd(std::path::Path::new("."))
            .register_extension(AgentExt)
            .build()
            .unwrap();
        rt.set_active_agent(Some("rlm")).unwrap();
        let err = rt.set_active_agent(Some("nope")).unwrap_err();
        assert!(err.to_string().contains("not registered"), "{err}");
        assert_eq!(rt.active_agent().as_deref(), Some("rlm"));
    }

    #[test]
    fn duplicate_agent_registration_is_rejected() {
        struct DupExt;
        impl Extension for DupExt {
            fn manifest(&self) -> ExtensionManifest {
                ExtensionManifest {
                    id: "test.dup.agent".to_owned(),
                    name: "DupAgent".to_owned(),
                    version: "0".to_owned(),
                    description: String::new(),
                }
            }
            fn register(&self, host: &mut dyn ExtensionHost) -> anyhow::Result<()> {
                host.register_agent(crate::AgentSpec::new("same"))?;
                host.register_agent(crate::AgentSpec::new("same"))?;
                Ok(())
            }
        }
        let err = match RuntimeBuilder::new()
            .with_cwd(std::path::Path::new("."))
            .register_extension(DupExt)
            .build()
        {
            Ok(_) => panic!("duplicate agent registration must fail the build"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("already registered"), "{err}");
    }
}
