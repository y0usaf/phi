use std::sync::Arc;

use anyhow::Result;
use phi_ext::{Extension, ExtensionHost, ExtensionManifest};

use crate::config::MemoryConfig;
use crate::store::MemoryStore;
use crate::tools;
use crate::commands;
use crate::events;

pub struct MemoryExtension {
    store:  Arc<MemoryStore>,
    config: MemoryConfig,
}

impl MemoryExtension {
    pub fn new() -> Result<Self> {
        Self::with_config(MemoryConfig::default())
    }

    pub fn with_config(config: MemoryConfig) -> Result<Self> {
        if !config.enabled {
            anyhow::bail!("memory extension disabled");
        }
        let store = Arc::new(MemoryStore::open(&config.vault_path)?);
        Ok(Self { store, config })
    }
}

impl Extension for MemoryExtension {
    fn manifest(&self) -> ExtensionManifest {
        ExtensionManifest {
            id: "builtin.memory".to_owned(),
            name: "Memory".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            description: "Cognitive memory vault: graph-linked notes with ACT-R decay,                           PageRank fusion, Hebbian learning, and recursive exploration.".to_owned(),
        }
    }

    fn register(&self, host: &mut dyn ExtensionHost) -> Result<()> {
        let store = &self.store;

        // -- Tools -------------------------------------------------------------
        host.register_tool(tools::recall::make_recall_tool(store))?;
        host.register_tool(tools::capture::make_capture_tool(store))?;
        host.register_tool(tools::link::make_link_tool(store))?;
        host.register_tool(tools::explore::make_explore_tool(store))?;
        host.register_tool(tools::reflect::make_reflect_tool(store))?;
        host.register_tool(tools::forget::make_forget_tool(store))?;
        host.register_tool(tools::graph::make_graph_tool(store))?;

        // -- Slash commands ----------------------------------------------------
        host.register_command(commands::make_recall_command(store))?;
        host.register_command(commands::make_memories_command(store))?;
        host.register_command(commands::make_capture_command(store))?;
        host.register_command(commands::make_explore_command(store))?;
        host.register_command(commands::make_memory_status_command(store))?;
        host.register_command(commands::make_forget_command(store))?;

        // -- Event hooks -------------------------------------------------------
        host.subscribe(events::make_before_agent_start_handler(
            store,
            self.config.recall_limit,
            self.config.auto_inject,
        ))?;
        host.subscribe(events::make_session_start_handler(store))?;

        Ok(())
    }
}


