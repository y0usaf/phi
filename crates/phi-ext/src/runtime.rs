use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::Mutex as TokioMutex;

use crate::{
    CommandInvocation, CommandOutput, CommandSpec, EventHandlerRegistration, EventKind,
    EventPayload, EventReturn, ExtensionContextRef, ExtensionManifest, ExtensionMode, FlagSpec,
    LifecycleEvent, MessageRendererSpec, OverlaySpec, ProviderConfig, ShortcutSpec, SpinnerSpec,
    TextEffectSpec, ThemeSpec, ThinkingExpressionSpec, ToolSpec,
};
use anyhow::{Result, anyhow};

use crate::snapshot::{
    EventHandlerSummary, RuntimeSnapshot, ThemeSummary, command_summary, flag_summary,
    overlay_summary, shortcut_summary, spinner_summary, text_effect_summary,
    thinking_expression_summary, tool_summary,
};

pub struct AppRuntime {
    cwd: PathBuf,
    session_id: String,
    extensions: Vec<ExtensionManifest>,
    pub(crate) tools: BTreeMap<String, ToolSpec>,
    pub(crate) commands: BTreeMap<String, CommandSpec>,
    pub(crate) shortcuts: Vec<ShortcutSpec>,
    pub(crate) flags: BTreeMap<String, FlagSpec>,
    pub(crate) event_handlers: Vec<EventHandlerRegistration>,
    pub(crate) message_renderers: BTreeMap<String, MessageRendererSpec>,
    pub(crate) providers: BTreeMap<String, ProviderConfig>,
    // ── Visual ──────────────────────────────────────────────────────────
    pub(crate) themes: BTreeMap<String, ThemeSpec>,
    pub(crate) spinners: BTreeMap<String, SpinnerSpec>,
    pub(crate) thinking_expressions: BTreeMap<String, ThinkingExpressionSpec>,
    pub(crate) text_effects: BTreeMap<String, TextEffectSpec>,
    pub(crate) overlays: BTreeMap<String, OverlaySpec>,
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
        tools: BTreeMap<String, ToolSpec>,
        commands: BTreeMap<String, CommandSpec>,
        shortcuts: Vec<ShortcutSpec>,
        flags: BTreeMap<String, FlagSpec>,
        event_handlers: Vec<EventHandlerRegistration>,
        message_renderers: BTreeMap<String, MessageRendererSpec>,
        providers: BTreeMap<String, ProviderConfig>,
        themes: BTreeMap<String, ThemeSpec>,
        spinners: BTreeMap<String, SpinnerSpec>,
        thinking_expressions: BTreeMap<String, ThinkingExpressionSpec>,
        text_effects: BTreeMap<String, TextEffectSpec>,
        overlays: BTreeMap<String, OverlaySpec>,
    ) -> Self {
        Self {
            cwd,
            session_id,
            extensions,
            tools,
            commands,
            shortcuts,
            flags,
            event_handlers,
            message_renderers,
            providers,
            themes,
            spinners,
            thinking_expressions,
            text_effects,
            overlays,
            notifications: Mutex::new(Vec::new()),
            status: TokioMutex::new(BTreeMap::new()),
            session_name: TokioMutex::new(None),
            labels: TokioMutex::new(BTreeMap::new()),
            active_tools: TokioMutex::new(Vec::new()),
            pending_messages: Mutex::new(Vec::new()),
        }
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
        let timeout = handler_timeout(event.kind);
        for reg in self.event_handlers.iter().filter(|h| h.event == event.kind) {
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

    pub async fn on_agent_end(&self, stop_cause: &str) -> Result<()> {
        self.dispatch(LifecycleEvent {
            kind: EventKind::AgentEnd,
            payload: EventPayload::AgentEnd {
                stop_cause: stop_cause.to_owned(),
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

    pub fn list_extension_commands(&self) -> Vec<(String, String)> {
        self.commands
            .values()
            .map(|c| (c.name.clone(), c.description.clone()))
            .collect()
    }

    /// Registry listing for completion popups and help output.
    pub fn list_commands(&self) -> Vec<crate::CommandInfo> {
        self.commands
            .values()
            .map(|c| crate::CommandInfo {
                name: c.name.clone(),
                args_hint: c.args_hint.clone(),
                description: c.description.clone(),
            })
            .collect()
    }

    /// First-argument completion candidates for a registered command, if the
    /// command declared a completion source.
    pub fn complete_command_arg(&self, name: &str) -> Option<Vec<String>> {
        self.commands
            .get(name)?
            .arg_completions
            .as_ref()
            .map(|f| f())
    }

    /// Invoke a command handler synchronously on the calling thread. The TUI
    /// uses this from its input path: handlers are expected to be quick (they
    /// return declarative `UiAction`s rather than doing UI work) and slow
    /// bridges (Lua) bound their own channel waits.
    pub fn invoke_command_sync(&self, name: &str, raw_args: &str) -> Result<Option<CommandOutput>> {
        let Some(cmd) = self.commands.get(name) else {
            return Ok(None);
        };
        (cmd.handler)(CommandInvocation {
            raw_args: raw_args.to_owned(),
        })
        .map(Some)
        .map_err(|e| anyhow!("command '{name}': {e}"))
    }

    pub async fn invoke_command(
        &self,
        name: &str,
        raw_args: &str,
    ) -> Result<Option<CommandOutput>> {
        let Some(cmd) = self.commands.get(name) else {
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
        let h = tool.handler.clone();
        let inv = crate::ToolInvocation {
            input: input.to_owned(),
        };
        Ok(Some(h(inv)?))
    }

    // ── Tool prelude stubs ────────────────────────────────────────────────────

    pub fn extension_prelude_stubs(&self) -> String {
        self.tools
            .values()
            .map(|t| {
                let sig: Vec<String> = t
                    .params
                    .iter()
                    .map(|p| match &p.default {
                        Some(def) => format!("{}: {} = {}", p.name, p.ty, def),
                        None => format!("{}: {}", p.name, p.ty),
                    })
                    .collect();
                let dict: Vec<String> = t
                    .params
                    .iter()
                    .map(|p| format!("\"{}\":  {}", p.name, p.name))
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
                    event: EventKind::ModelSelect,
                    label: "test.multi.err".to_owned(),
                    handler: Arc::new(|_ev| Err(anyhow::anyhow!("boom"))),
                })?;
                host.subscribe(EventHandlerRegistration {
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
}
