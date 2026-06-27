use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use tokio::sync::Mutex as TokioMutex;

use anyhow::{Result, anyhow};
use phi_ext::{
    CommandInvocation, CommandOutput, CommandSpec, EventHandlerRegistration, EventKind,
    EventPayload, EventReturn, ExtensionManifest, LifecycleEvent, ToolSpec,
};

use crate::snapshot::{EventHandlerSummary, RuntimeSnapshot, command_summary, tool_summary};

pub struct AppRuntime {
    cwd: PathBuf,
    session_id: String,
    extensions: Vec<ExtensionManifest>,
    pub(crate) tools: BTreeMap<String, ToolSpec>,
    pub(crate) commands: BTreeMap<String, CommandSpec>,
    pub(crate) event_handlers: Vec<EventHandlerRegistration>,
    notifications: Mutex<Vec<String>>,
    status: TokioMutex<BTreeMap<String, String>>,
}

impl AppRuntime {
    pub(crate) fn from_builder(
        cwd: PathBuf,
        session_id: String,
        extensions: Vec<ExtensionManifest>,
        tools: BTreeMap<String, ToolSpec>,
        commands: BTreeMap<String, CommandSpec>,
        event_handlers: Vec<EventHandlerRegistration>,
    ) -> Self {
        Self {
            cwd,
            session_id,
            extensions,
            tools,
            commands,
            event_handlers,
            notifications: Mutex::new(Vec::new()),
            status: TokioMutex::new(BTreeMap::new()),
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

    // ── Lifecycle helpers ─────────────────────────────────────────────────────

    pub async fn boot(&self) -> Result<()> {
        self.dispatch(LifecycleEvent {
            kind: EventKind::RuntimeStart,
            payload: EventPayload::Empty,
        })
        .await?;
        Ok(())
    }

    pub async fn start_session(&self) -> Result<()> {
        self.dispatch(LifecycleEvent {
            kind: EventKind::SessionStart,
            payload: EventPayload::SessionStart {
                cwd: self.cwd.display().to_string(),
                session_id: self.session_id.clone(),
            },
        })
        .await?;
        Ok(())
    }

    pub async fn shutdown_session(&self) -> Result<()> {
        self.dispatch(LifecycleEvent {
            kind: EventKind::SessionShutdown,
            payload: EventPayload::Empty,
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

    pub fn invoke_tool_sync(
        &self,
        name: &str,
        input: &str,
    ) -> Result<Option<phi_ext::ToolOutput>> {
        let Some(tool) = self.tools.get(name) else {
            return Ok(None);
        };
        let h = tool.handler.clone();
        let inv = phi_ext::ToolInvocation {
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

pub fn missing_named_item(kind: &str, name: &str) -> anyhow::Error {
    anyhow!("{kind} '{name}' is not registered")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use phi_ext::{
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
}
