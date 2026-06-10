use std::collections::BTreeMap;
use std::env;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow, bail};
use phi_ext::{
    CommandInvocation, CommandOutput, CommandSpec, EventContext, EventHandlerRegistration,
    EventKind, Extension, ExtensionHost, ExtensionManifest, LifecycleEvent, ToolInvocation,
    ToolOutput, ToolSpec,
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
}

impl AppRuntime {
    pub fn boot(&self) -> Result<()> {
        self.dispatch(EventKind::RuntimeStart, "runtime boot")
    }

    pub fn start_session(&self) -> Result<()> {
        self.dispatch(EventKind::SessionStart, "session start")
    }

    pub fn dispatch(&self, kind: EventKind, summary: impl Into<String>) -> Result<()> {
        let event = LifecycleEvent {
            kind,
            summary: summary.into(),
        };

        let ctx = EventContext {
            cwd: self.cwd.display().to_string(),
            session_id: self.session_id.clone(),
        };

        for handler in self.event_handlers.iter().filter(|h| h.event == kind) {
            (handler.handler)(&ctx, &event)?;
        }

        Ok(())
    }

    pub fn invoke_command(&self, name: &str, raw_args: &str) -> Result<Option<CommandOutput>> {
        let Some(command) = self.commands.get(name) else {
            return Ok(None);
        };

        (command.handler)(CommandInvocation {
            raw_args: raw_args.to_owned(),
        })
        .map(Some)
    }

    pub fn invoke_tool(&self, name: &str, input: impl Into<String>) -> Result<Option<ToolOutput>> {
        let Some(tool) = self.tools.get(name) else {
            return Ok(None);
        };

        self.dispatch(EventKind::BeforeTool, format!("tool {name}"))?;
        let output = (tool.handler)(ToolInvocation {
            input: input.into(),
        })?;
        self.dispatch(EventKind::AfterTool, format!("tool {name}"))?;

        Ok(Some(output))
    }

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
