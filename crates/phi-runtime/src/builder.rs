use std::env;
use std::path::{Path, PathBuf};

use anyhow::Result;
use phi_ext::Extension;

use crate::host::RegistryHost;
use crate::runtime::AppRuntime;

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

    pub fn register_extension<E>(self, extension: E) -> Self
    where
        E: Extension + 'static,
    {
        self.register_boxed_extension(Box::new(extension))
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

        Ok(AppRuntime::from_builder(
            self.cwd,
            self.session_id,
            manifests,
            host.tools,
            host.commands,
            host.event_handlers,
        ))
    }
}

fn default_session_id() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();

    format!("session-{millis}")
}
