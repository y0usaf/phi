use std::sync::Arc;

use phi_ext::{CommandInvocation, CommandOutput, CommandSpec};

use crate::store::MemoryStore;
use crate::format::{format_memories, format_explored};

pub fn make_recall_command(store: &Arc<MemoryStore>) -> CommandSpec {
    let store = Arc::clone(store);
    CommandSpec {
        name: "/recall".to_owned(),
        description: "Search memory (fusion) -- /recall <query>".to_owned(),
        handler: Arc::new(move |inv: CommandInvocation| {
            let query = inv.raw_args.trim().to_owned();
            if query.is_empty() {
                return Ok(CommandOutput { message: Some("Usage: /recall <query>".to_owned()) });
            }
            let memories = store.recall(&query, 10)?;
            Ok(CommandOutput { message: Some(format_memories(&memories)) })
        }),
    }
}

pub fn make_memories_command(store: &Arc<MemoryStore>) -> CommandSpec {
    let store = Arc::clone(store);
    CommandSpec {
        name: "/memories".to_owned(),
        description: "List recent memories -- /memories [insight|note|goal|context]".to_owned(),
        handler: Arc::new(move |inv: CommandInvocation| {
            let kind_arg = inv.raw_args.trim().to_owned();
            let kind = if kind_arg.is_empty() { None } else { Some(kind_arg.as_str()) };
            let memories = store.list(kind, 20)?;
            Ok(CommandOutput { message: Some(format_memories(&memories)) })
        }),
    }
}

pub fn make_capture_command(store: &Arc<MemoryStore>) -> CommandSpec {
    let store = Arc::clone(store);
    CommandSpec {
        name: "/capture".to_owned(),
        description: "Save a memory -- /capture <title> -- <body>".to_owned(),
        handler: Arc::new(move |inv: CommandInvocation| {
            let raw = inv.raw_args.trim().to_owned();
            if let Some((title, body)) = raw.split_once(" -- ") {
                let id = store.capture(title.trim(), body.trim(), "note")?;
                Ok(CommandOutput { message: Some(format!("Saved memory #{id}")) })
            } else {
                Ok(CommandOutput { message: Some("Usage: /capture <title> -- <body>".to_owned()) })
            }
        }),
    }
}

pub fn make_explore_command(store: &Arc<MemoryStore>) -> CommandSpec {
    let store = Arc::clone(store);
    CommandSpec {
        name: "/explore".to_owned(),
        description: "Graph walk -- /explore <query>".to_owned(),
        handler: Arc::new(move |inv: CommandInvocation| {
            let query = inv.raw_args.trim().to_owned();
            if query.is_empty() {
                return Ok(CommandOutput { message: Some("Usage: /explore <query>".to_owned()) });
            }
            let discovered = store.explore(&query, 10)?;
            Ok(CommandOutput { message: Some(format_explored(&discovered)) })
        }),
    }
}

pub fn make_memory_status_command(store: &Arc<MemoryStore>) -> CommandSpec {
    let store = Arc::clone(store);
    CommandSpec {
        name: "/memory-status".to_owned(),
        description: "Show memory vault + graph statistics".to_owned(),
        handler: Arc::new(move |_inv: CommandInvocation| {
            let (count, last) = store.status()?;
            let (edges, avg_strength) = store.graph_status()?;
            let last_str = last
                .map(|ts| format!("{ts}"))
                .unwrap_or_else(|| "never".to_owned());
            Ok(CommandOutput {
                message: Some(format!(
                    "Memory vault: {count} memories | {edges} edges | avg strength: {avg_strength:.3} | last capture: {last_str}"
                ))
            })
        }),
    }
}

pub fn make_forget_command(store: &Arc<MemoryStore>) -> CommandSpec {
    let store = Arc::clone(store);
    CommandSpec {
        name: "/forget".to_owned(),
        description: "Delete a memory -- /forget <id>".to_owned(),
        handler: Arc::new(move |inv: CommandInvocation| {
            let raw = inv.raw_args.trim();
            let id: i64 = raw.parse().unwrap_or(0);
            if id == 0 {
                return Ok(CommandOutput { message: Some("Usage: /forget <id>".to_owned()) });
            }
            store.forget(id)?;
            Ok(CommandOutput { message: Some(format!("Forgot memory #{id}")) })
        }),
    }
}


