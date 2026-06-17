use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow, bail};
use phi_ext::{
    CommandInvocation, CommandOutput, CommandSpec, EventHandlerRegistration,
    EventKind, EventPayload, EventReturn, Extension, ExtensionHost, ExtensionManifest,
    LifecycleEvent, ToolSpec,
};
use serde::Serialize;

pub struct RuntimeBuilder {
    cwd: PathBuf,
    session_id: String,
    extensions: Vec<Box<dyn Extension>>,
}

impl Default for RuntimeBuilder {
    fn default() -> Self {
        Self {
            cwd: env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            session_id: default_session_id(),
            extensions: Vec::new(),
        }
    }
}

impl RuntimeBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_cwd(mut self, cwd: &Path) -> Self {
        self.cwd = cwd.to_owned();
        self
    }

    pub fn with_session_id(mut self, id: impl Into<String>) -> Self {
        self.session_id = id.into();
        self
    }

    pub fn register_extension<E>(mut self, extension: E) -> Self
    where
        E: Extension + 'static,
    {
        self.extensions.push(Box::new(extension));
        self
    }

    pub fn register_boxed_extension(mut self, extension: Box<dyn Extension>) -> Self {
        self.extensions.push(extension);
        self
    }

    pub fn build(self) -> Result<AppRuntime> {
        let mut host = RegistryHost::default();
        let mut manifests = Vec::new();

        for extension in &self.extensions {
            manifests.push(extension.manifest());
            extension.register(&mut host)?;
        }

        Ok(AppRuntime {
            cwd: self.cwd,
            session_id: self.session_id,
            extensions: manifests,
            tools: host.tools,
            commands: host.commands,
            event_handlers: host.event_handlers,
            notifications: Mutex::new(Vec::new()),
            status: Mutex::new(BTreeMap::new()),
        })
    }
}

#[derive(Default)]
struct RegistryHost {
    tools: BTreeMap<String, ToolSpec>,
    commands: BTreeMap<String, CommandSpec>,
    event_handlers: Vec<EventHandlerRegistration>,
}

impl ExtensionHost for RegistryHost {
    fn register_tool(&mut self, tool: ToolSpec) -> Result<()> {
        if self.tools.contains_key(&tool.name) {
            bail!("tool '{}' is already registered", tool.name);
        }
        self.tools.insert(tool.name.clone(), tool);
        Ok(())
    }

    fn register_command(&mut self, command: CommandSpec) -> Result<()> {
        if self.commands.contains_key(&command.name) {
            bail!("command '{}' is already registered", command.name);
        }
        self.commands.insert(command.name.clone(), command);
        Ok(())
    }

    fn subscribe(&mut self, handler: EventHandlerRegistration) -> Result<()> {
        self.event_handlers.push(handler);
        Ok(())
    }
}

pub struct AppRuntime {
    cwd: PathBuf,
    session_id: String,
    extensions: Vec<ExtensionManifest>,
    tools: BTreeMap<String, ToolSpec>,
    commands: BTreeMap<String, CommandSpec>,
    event_handlers: Vec<EventHandlerRegistration>,
    notifications: Mutex<Vec<String>>,
    status: Mutex<BTreeMap<String, String>>,
}

impl AppRuntime {
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

    pub async fn filter_system_prompt(&self, prompt: &str, system_prompt: String) -> Result<String> {
        let returns = self.dispatch(LifecycleEvent {
            kind: EventKind::BeforeAgentStart,
            payload: EventPayload::BeforeAgentStart {
                prompt: prompt.to_owned(),
                system_prompt: system_prompt.clone(),
            },
        }).await?;

        let mut result = system_prompt;
        for r in returns {
            if let EventReturn::SystemPromptMod { prepend, append } = r {
                if let Some(pre) = prepend {
                    result = format!("{pre}\n\n{result}");
                }
                if let Some(app) = append {
                    result.push_str(&format!("\n\n{app}"));
                }
            }
        }
        Ok(result)
    }

    // ── Context injection ─────────────────────────────────────────────────────

    pub async fn context_injections(&self, turn: u32) -> Result<Vec<(String, String)>> {
        let returns = self.dispatch(LifecycleEvent {
            kind: EventKind::Context,
            payload: EventPayload::Context { turn },
        }).await?;

        Ok(returns.into_iter().filter_map(|r| {
            if let EventReturn::MessageInjection { prepend } = r {
                Some(prepend)
            } else {
                None
            }
        }).flatten().collect())
    }

    // ── Lifecycle helpers ─────────────────────────────────────────────────────

    pub async fn boot(&self) -> Result<()> {
        self.dispatch(LifecycleEvent {
            kind: EventKind::RuntimeStart,
            payload: EventPayload::Empty,
        }).await?;
        Ok(())
    }

    pub async fn start_session(&self) -> Result<()> {
        self.dispatch(LifecycleEvent {
            kind: EventKind::SessionStart,
            payload: EventPayload::SessionStart {
                cwd: self.cwd.display().to_string(),
                session_id: self.session_id.clone(),
            },
        }).await?;
        Ok(())
    }

    pub async fn shutdown_session(&self) -> Result<()> {
        self.dispatch(LifecycleEvent {
            kind: EventKind::SessionShutdown,
            payload: EventPayload::Empty,
        }).await?;
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

    pub fn set_status(&self, key: impl Into<String>, text: impl Into<String>) {
        self.status.lock().unwrap().insert(key.into(), text.into());
    }

    pub fn clear_status(&self, key: impl Into<String>) {
        self.status.lock().unwrap().remove(&key.into());
    }

    pub fn status_snapshot(&self) -> Vec<(String, String)> {
        self.status
            .lock()
            .unwrap()
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

    pub async fn invoke_command(&self, name: &str, raw_args: &str) -> Result<Option<CommandOutput>> {
        let Some(cmd) = self.commands.get(name) else {
            return Ok(None);
        };
        let h = cmd.handler.clone();
        let args = raw_args.to_owned();
        let output = tokio::task::spawn_blocking(move || h(CommandInvocation { raw_args: args })).await??;
        Ok(Some(output))
    }

    // ── Tool invocation ───────────────────────────────────────────────────────

    pub fn invoke_tool_sync(&self, name: &str, input: &str) -> anyhow::Result<Option<phi_ext::ToolOutput>> {
        let Some(tool) = self.tools.get(name) else {
            return Ok(None);
        };
        let h = tool.handler.clone();
        let inv = phi_ext::ToolInvocation { input: input.to_owned() };
        Ok(Some(h(inv)?))
    }

    // ── Tool prelude stubs ────────────────────────────────────────────────────

    pub fn extension_prelude_stubs(&self) -> String {
        self.tools.values().map(|t| {
            let sig: Vec<String> = t.params.iter().map(|p| {
                match &p.default {
                    Some(def) => format!("{}: {} = {}", p.name, p.ty, def),
                    None      => format!("{}: {}", p.name, p.ty),
                }
            }).collect();
            let dict: Vec<String> = t.params.iter()
                .map(|p| format!("\"{}\":  {}", p.name, p.name))
                .collect();
            format!(
                "def {}({}) -> str:\n    return __phi_ext_tool__(\"{}\" , {{{}}})",
                t.name,
                sig.join(", "),
                t.name,
                dict.join(", ")
            )
        }).collect::<Vec<_>>().join("\n\n")
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

// ── Summary types ─────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize)]
pub struct RuntimeSnapshot {
    pub cwd: String,
    pub session_id: String,
    pub extensions: Vec<ExtensionManifest>,
    pub tools: Vec<ToolSummary>,
    pub commands: Vec<CommandSummary>,
    pub event_handlers: Vec<EventHandlerSummary>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ToolSummary {
    pub name: String,
    pub description: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct CommandSummary {
    pub name: String,
    pub description: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct EventHandlerSummary {
    pub event: EventKind,
    pub label: String,
}

fn tool_summary(tool: &ToolSpec) -> ToolSummary {
    ToolSummary {
        name: tool.name.clone(),
        description: tool.description.clone(),
    }
}

fn command_summary(command: &CommandSpec) -> CommandSummary {
    CommandSummary {
        name: command.name.clone(),
        description: command.description.clone(),
    }
}

fn default_session_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();

    format!("session-{millis}")
}

pub fn missing_named_item(kind: &str, name: &str) -> anyhow::Error {
    anyhow!("{kind} '{name}' is not registered")
}
