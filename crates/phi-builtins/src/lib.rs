pub mod prelude;

use std::sync::Arc;

use anyhow::Result;
use phi_ext::{
    CommandOutput, CommandSpec, EventHandlerRegistration, EventKind, Extension, ExtensionHost,
    ExtensionManifest, ToolOutput, ToolSpec,
};

pub fn bundled_extensions() -> Vec<Box<dyn Extension>> {
    vec![
        Box::new(CoreMetaExtension),
        Box::new(ScaffoldLifecycleExtension),
    ]
}

struct CoreMetaExtension;

impl Extension for CoreMetaExtension {
    fn manifest(&self) -> ExtensionManifest {
        ExtensionManifest {
            id: "builtin.core-meta".to_owned(),
            name: "Core Meta".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            description: "Registers scaffold commands and placeholder tools.".to_owned(),
        }
    }

    fn register(&self, host: &mut dyn ExtensionHost) -> Result<()> {
        host.register_command(CommandSpec {
            name: "about".to_owned(),
            description: "Describe the scaffolded harness.".to_owned(),
            handler: Arc::new(|_invocation| {
                Ok(CommandOutput {
                    message: Some(
                        "phi is currently a scaffolded extension-first runtime with native CLI/TUI boundaries.".to_owned(),
                    ),
                })
            }),
        })?;

        host.register_tool(ToolSpec {
            name: "echo".to_owned(),
            description: "Echo the provided tool input.".to_owned(),
            handler: Arc::new(|invocation| {
                Ok(ToolOutput {
                    content: invocation.input,
                })
            }),
        })?;

        Ok(())
    }
}

struct ScaffoldLifecycleExtension;

impl Extension for ScaffoldLifecycleExtension {
    fn manifest(&self) -> ExtensionManifest {
        ExtensionManifest {
            id: "builtin.lifecycle".to_owned(),
            name: "Lifecycle Scaffold".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            description: "Registers example lifecycle hooks for the native harness.".to_owned(),
        }
    }

    fn register(&self, host: &mut dyn ExtensionHost) -> Result<()> {
        host.subscribe(EventHandlerRegistration {
            event: EventKind::RuntimeStart,
            label: "builtin.lifecycle.runtime-start".to_owned(),
            handler: Arc::new(|_ctx, _event| Ok(())),
        })?;

        host.subscribe(EventHandlerRegistration {
            event: EventKind::TuiReady,
            label: "builtin.lifecycle.tui-ready".to_owned(),
            handler: Arc::new(|_ctx, _event| Ok(())),
        })?;

        Ok(())
    }
}
