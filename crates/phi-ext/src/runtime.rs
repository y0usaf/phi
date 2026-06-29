use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use tokio::sync::Mutex as TokioMutex;

use crate::{
    CommandInvocation, CommandOutput, CommandSpec, EventHandlerRegistration, EventKind,
    EventPayload, EventReturn, ExtensionContextRef, ExtensionMode, ExtensionManifest,
    FlagSpec, LifecycleEvent, MessageRendererSpec, ProviderConfig, ShortcutSpec, ToolSpec,
};
use anyhow::{Result, anyhow};

use crate::snapshot::{
    EventHandlerSummary, RuntimeSnapshot, command_summary, flag_summary, shortcut_summary,
    tool_summary,
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
    notifications: Mutex<Vec<String>>,
    status: TokioMutex<BTreeMap<String, String>>,
    session_name: TokioMutex<Option<String>>,
    labels: TokioMutex<BTreeMap<String, String>>,
    active_tools: TokioMutex<Vec<String>>,
    pending_messages: Mutex<Vec<String>>,
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
        for reg in self.event_handlers.iter().filter(|h| h.event == event.kind) {
            let h = reg.handler.clone();
            let ev = event.clone();
            let result = tokio::task::spawn_blocking(move || h(ev)).await??;
            if let Some(r) = result {
                returns.push(r);
            }
        }
        Ok(returns)
    }

    /// Dispatch an event and check if any handler returned `Cancel`.
    pub async fn dispatch_cancelable(&self, event: LifecycleEvent) -> Result<bool> {
        let returns = self.dispatch(event).await?;
        Ok(returns.iter().any(|r| matches!(r, EventReturn::Cancel { .. })))
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
            if let EventReturn::SystemPromptMod { prepend, append } = r {
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

    pub async fn before_provider_request(&self, payload: serde_json::Value) -> Result<serde_json::Value> {
        let returns = self
            .dispatch(LifecycleEvent {
                kind: EventKind::BeforeProviderRequest,
                payload: EventPayload::BeforeProviderRequest { payload: payload.clone() },
            })
            .await?;

        let mut result = payload;
        for r in returns {
            if let EventReturn::ProviderRequestOverride { payload: override_payload } = r {
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

    pub async fn before_session_fork(&self, entry_id: &str, position: &str) -> Result<bool> {
        self.dispatch_cancelable(LifecycleEvent {
            kind: EventKind::SessionBeforeFork,
            payload: EventPayload::SessionBeforeFork {
                entry_id: entry_id.to_owned(),
                position: position.to_owned(),
            },
        })
        .await
    }

    pub async fn before_compact(&self, custom_instructions: Option<&str>) -> Result<Option<String>> {
        let returns = self
            .dispatch(LifecycleEvent {
                kind: EventKind::SessionBeforeCompact,
                payload: EventPayload::SessionBeforeCompact {
                    custom_instructions: custom_instructions.map(str::to_owned),
                },
            })
            .await?;

        if returns.iter().any(|r| matches!(r, EventReturn::Cancel { .. })) {
            return Ok(None);
        }
        for r in returns {
            if let EventReturn::CompactCustom { instructions } = r {
                return Ok(Some(instructions));
            }
        }
        Ok(Some(custom_instructions.unwrap_or("").to_owned()))
    }

    // ── Message interception ────────────────────────────────────────────────────

    pub async fn on_message_end(&self, message: serde_json::Value) -> Result<Option<serde_json::Value>> {
        let returns = self
            .dispatch(LifecycleEvent {
                kind: EventKind::MessageEnd,
                payload: EventPayload::MessageEnd { message: message.clone() },
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

    // ── Commands ──────────────────────────────────────────────────────────────

    pub fn list_extension_commands(&self) -> Vec<(String, String)> {
        self.commands
            .values()
            .map(|c| (c.name.clone(), c.description.clone()))
            .collect()
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
        let output =
            tokio::task::spawn_blocking(move || h(CommandInvocation { raw_args: args })).await??;
        Ok(Some(output))
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
        }
    }
}

// ── Result types ────────────────────────────────────────────────────────────

/// Result of project trust check.
pub struct ProjectTrustDecision {
    pub trusted: bool,
    pub remember: bool,
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

        let decision = rt
            .check_project_trust(".")
            .await
            .expect("trust check");
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

        let instructions = rt
            .before_compact(None)
            .await
            .expect("before compact");
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

        let instructions = rt
            .before_compact(None)
            .await
            .expect("before compact");
        assert!(instructions.is_none());
    }
}
