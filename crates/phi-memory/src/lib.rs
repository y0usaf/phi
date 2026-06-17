mod store;
pub use store::{Edge, FusionWeights, Memory, MemoryStore};

use std::sync::Arc;
use anyhow::Result;
use phi_ext::{
    CommandInvocation, CommandOutput, CommandSpec, EventHandlerRegistration,
    EventKind, EventPayload, EventReturn, Extension, ExtensionHost, ExtensionManifest,
    ToolInvocation, ToolOutput, ToolParam, ToolSpec,
};

#[derive(Debug, Clone)]
pub struct MemoryConfig {
    pub enabled:      bool,
    pub vault_path:   std::path::PathBuf,
    pub recall_limit: usize,
    pub auto_inject:  bool,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled:      true,
            vault_path:   dirs::data_local_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
                .join("phi")
                .join("memory.db"),
            recall_limit: 5,
            auto_inject:  true,
        }
    }
}

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
        let store = Arc::clone(&self.store);
        let recall_limit = self.config.recall_limit;
        let auto_inject  = self.config.auto_inject;

        // -- Tools (LLM-callable via REPL prelude stubs) -----------------------

        // memory_recall(query, limit=5)
        host.register_tool(ToolSpec {
            name: "memory_recall".to_owned(),
            description: "Search your persistent memory vault with hybrid BM25+graph+decay fusion.".to_owned(),
            params: vec![
                ToolParam { name: "query".to_owned(), ty: "str".to_owned(), default: None },
                ToolParam { name: "limit".to_owned(), ty: "int".to_owned(), default: Some("5".to_owned()) },
            ],
            handler: Arc::new({
                let store = Arc::clone(&store);
                move |inv: ToolInvocation| {
                    let v: serde_json::Value = serde_json::from_str(&inv.input).unwrap_or_default();
                    let query = v["query"].as_str().unwrap_or("").to_owned();
                    let limit = v["limit"].as_u64().unwrap_or(5) as usize;
                    if query.is_empty() {
                        return Ok(ToolOutput { content: "memory_recall: query is required".to_owned() });
                    }
                    let memories = store.recall(&query, limit)?;
                    Ok(ToolOutput { content: format_memories(&memories) })
                }
            }),
        })?;

        // memory_capture(title, body, kind="note")
        host.register_tool(ToolSpec {
            name: "memory_capture".to_owned(),
            description: "Save an insight/decision/note to memory. [[wiki-links]] auto-link into the graph.".to_owned(),
            params: vec![
                ToolParam { name: "title".to_owned(), ty: "str".to_owned(), default: None },
                ToolParam { name: "body".to_owned(),  ty: "str".to_owned(), default: None },
                ToolParam { name: "kind".to_owned(),  ty: "str".to_owned(), default: Some("\"note\"".to_owned()) },
            ],
            handler: Arc::new({
                let store = Arc::clone(&store);
                move |inv: ToolInvocation| {
                    let v: serde_json::Value = serde_json::from_str(&inv.input).unwrap_or_default();
                    let title = v["title"].as_str().unwrap_or("").to_owned();
                    let body  = v["body"].as_str().unwrap_or("").to_owned();
                    let kind  = v["kind"].as_str().unwrap_or("note").to_owned();
                    if title.is_empty() || body.is_empty() {
                        return Ok(ToolOutput { content: "memory_capture: title and body are required".to_owned() });
                    }
                    let id = store.capture(&title, &body, &kind)?;
                    Ok(ToolOutput { content: format!("Captured memory #{id}: {title}") })
                }
            }),
        })?;

        // memory_link(source, target, relation="related", weight=0.5)
        host.register_tool(ToolSpec {
            name: "memory_link".to_owned(),
            description: "Create an explicit typed edge between two memories in the knowledge graph.".to_owned(),
            params: vec![
                ToolParam { name: "source".to_owned(),   ty: "int".to_owned(), default: None },
                ToolParam { name: "target".to_owned(),   ty: "int".to_owned(), default: None },
                ToolParam { name: "relation".to_owned(), ty: "str".to_owned(), default: Some("\"related\"".to_owned()) },
                ToolParam { name: "weight".to_owned(),   ty: "float".to_owned(), default: Some("0.5".to_owned()) },
            ],
            handler: Arc::new({
                let store = Arc::clone(&store);
                move |inv: ToolInvocation| {
                    let v: serde_json::Value = serde_json::from_str(&inv.input).unwrap_or_default();
                    let source   = v["source"].as_i64().unwrap_or(0);
                    let target   = v["target"].as_i64().unwrap_or(0);
                    let relation = v["relation"].as_str().unwrap_or("related").to_owned();
                    let weight   = v["weight"].as_f64().unwrap_or(0.5);
                    if source == 0 || target == 0 {
                        return Ok(ToolOutput { content: "memory_link: source and target ids are required".to_owned() });
                    }
                    store.link(source, target, &relation, weight)?;
                    Ok(ToolOutput { content: format!("Linked #{source} --[{relation}]--> #{target}") })
                }
            }),
        })?;

        // memory_explore(query, limit=5)
        host.register_tool(ToolSpec {
            name: "memory_explore".to_owned(),
            description: "Recursively explore the graph neighborhood around BM25 seeds (PPR walk).".to_owned(),
            params: vec![
                ToolParam { name: "query".to_owned(), ty: "str".to_owned(), default: None },
                ToolParam { name: "limit".to_owned(), ty: "int".to_owned(), default: Some("5".to_owned()) },
            ],
            handler: Arc::new({
                let store = Arc::clone(&store);
                move |inv: ToolInvocation| {
                    let v: serde_json::Value = serde_json::from_str(&inv.input).unwrap_or_default();
                    let query = v["query"].as_str().unwrap_or("").to_owned();
                    let limit = v["limit"].as_u64().unwrap_or(5) as usize;
                    if query.is_empty() {
                        return Ok(ToolOutput { content: "memory_explore: query is required".to_owned() });
                    }
                    let discovered = store.explore(&query, limit)?;
                    if discovered.is_empty() {
                        return Ok(ToolOutput { content: "No related memories discovered in the graph.".to_owned() });
                    }
                    Ok(ToolOutput { content: format_explored(&discovered) })
                }
            }),
        })?;

        // memory_reflect(id, reward=0.5)
        host.register_tool(ToolSpec {
            name: "memory_reflect".to_owned(),
            description: "Reinforce a memory after it proved useful (Q-learning strength update).".to_owned(),
            params: vec![
                ToolParam { name: "id".to_owned(),     ty: "int".to_owned(),   default: None },
                ToolParam { name: "reward".to_owned(), ty: "float".to_owned(), default: Some("0.5".to_owned()) },
            ],
            handler: Arc::new({
                let store = Arc::clone(&store);
                move |inv: ToolInvocation| {
                    let v: serde_json::Value = serde_json::from_str(&inv.input).unwrap_or_default();
                    let id     = v["id"].as_i64().unwrap_or(0);
                    let reward = v["reward"].as_f64().unwrap_or(0.5);
                    if id == 0 {
                        return Ok(ToolOutput { content: "memory_reflect: id is required".to_owned() });
                    }
                    store.reflect(id, reward)?;
                    Ok(ToolOutput { content: format!("Reinforced memory #{id} (reward={reward})") })
                }
            }),
        })?;

        // memory_forget(id)  -- hard delete
        host.register_tool(ToolSpec {
            name: "memory_forget".to_owned(),
            description: "Permanently delete a memory and its edges from the vault.".to_owned(),
            params: vec![
                ToolParam { name: "id".to_owned(), ty: "int".to_owned(), default: None },
            ],
            handler: Arc::new({
                let store = Arc::clone(&store);
                move |inv: ToolInvocation| {
                    let v: serde_json::Value = serde_json::from_str(&inv.input).unwrap_or_default();
                    let id = v["id"].as_i64().unwrap_or(0);
                    if id == 0 {
                        return Ok(ToolOutput { content: "memory_forget: id is required".to_owned() });
                    }
                    store.forget(id)?;
                    Ok(ToolOutput { content: format!("Forgot memory #{id}") })
                }
            }),
        })?;

        // memory_graph(id)  -- show edges of a memory
        host.register_tool(ToolSpec {
            name: "memory_graph".to_owned(),
            description: "Show the knowledge-graph edges attached to a memory.".to_owned(),
            params: vec![
                ToolParam { name: "id".to_owned(), ty: "int".to_owned(), default: None },
            ],
            handler: Arc::new({
                let store = Arc::clone(&store);
                move |inv: ToolInvocation| {
                    let v: serde_json::Value = serde_json::from_str(&inv.input).unwrap_or_default();
                    let id = v["id"].as_i64().unwrap_or(0);
                    if id == 0 {
                        return Ok(ToolOutput { content: "memory_graph: id is required".to_owned() });
                    }
                    let edges = store.edges_of(id)?;
                    Ok(ToolOutput { content: format_edges(&edges) })
                }
            }),
        })?;

        // -- Slash commands ----------------------------------------------------

        // /recall <query>
        host.register_command(CommandSpec {
            name: "/recall".to_owned(),
            description: "Search memory (fusion) -- /recall <query>".to_owned(),
            handler: Arc::new({
                let store = Arc::clone(&store);
                move |inv: CommandInvocation| {
                    let query = inv.raw_args.trim().to_owned();
                    if query.is_empty() {
                        return Ok(CommandOutput { message: Some("Usage: /recall <query>".to_owned()) });
                    }
                    let memories = store.recall(&query, 10)?;
                    Ok(CommandOutput { message: Some(format_memories(&memories)) })
                }
            }),
        })?;

        // /memories [kind]
        host.register_command(CommandSpec {
            name: "/memories".to_owned(),
            description: "List recent memories -- /memories [insight|note|goal|context]".to_owned(),
            handler: Arc::new({
                let store = Arc::clone(&store);
                move |inv: CommandInvocation| {
                    let kind_arg = inv.raw_args.trim().to_owned();
                    let kind = if kind_arg.is_empty() { None } else { Some(kind_arg.as_str()) };
                    let memories = store.list(kind, 20)?;
                    Ok(CommandOutput { message: Some(format_memories(&memories)) })
                }
            }),
        })?;

        // /capture <title> -- <body>
        host.register_command(CommandSpec {
            name: "/capture".to_owned(),
            description: "Save a memory -- /capture <title> -- <body>".to_owned(),
            handler: Arc::new({
                let store = Arc::clone(&store);
                move |inv: CommandInvocation| {
                    let raw = inv.raw_args.trim().to_owned();
                    if let Some((title, body)) = raw.split_once(" -- ") {
                        let id = store.capture(title.trim(), body.trim(), "note")?;
                        Ok(CommandOutput { message: Some(format!("Saved memory #{id}")) })
                    } else {
                        Ok(CommandOutput { message: Some("Usage: /capture <title> -- <body>".to_owned()) })
                    }
                }
            }),
        })?;

        // /explore <query>
        host.register_command(CommandSpec {
            name: "/explore".to_owned(),
            description: "Graph walk -- /explore <query>".to_owned(),
            handler: Arc::new({
                let store = Arc::clone(&store);
                move |inv: CommandInvocation| {
                    let query = inv.raw_args.trim().to_owned();
                    if query.is_empty() {
                        return Ok(CommandOutput { message: Some("Usage: /explore <query>".to_owned()) });
                    }
                    let discovered = store.explore(&query, 10)?;
                    Ok(CommandOutput { message: Some(format_explored(&discovered)) })
                }
            }),
        })?;

        // /memory-status
        host.register_command(CommandSpec {
            name: "/memory-status".to_owned(),
            description: "Show memory vault + graph statistics".to_owned(),
            handler: Arc::new({
                let store = Arc::clone(&store);
                move |_inv: CommandInvocation| {
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
                }
            }),
        })?;

        // /forget <id>
        host.register_command(CommandSpec {
            name: "/forget".to_owned(),
            description: "Delete a memory -- /forget <id>".to_owned(),
            handler: Arc::new({
                let store = Arc::clone(&store);
                move |inv: CommandInvocation| {
                    let raw = inv.raw_args.trim();
                    let id: i64 = raw.parse().unwrap_or(0);
                    if id == 0 {
                        return Ok(CommandOutput { message: Some("Usage: /forget <id>".to_owned()) });
                    }
                    store.forget(id)?;
                    Ok(CommandOutput { message: Some(format!("Forgot memory #{id}")) })
                }
            }),
        })?;

        // -- Event hooks -------------------------------------------------------

        // BeforeAgentStart: inject top recalled memories into system prompt.
        // Uses keyword extraction instead of the raw prompt so FTS5 OR-queries
        // hit real signal rather than stopwords.
        host.subscribe(EventHandlerRegistration {
            event: EventKind::BeforeAgentStart,
            label: "builtin.memory.inject".to_owned(),
            handler: Arc::new({
                let store = Arc::clone(&store);
                move |event: phi_ext::LifecycleEvent| {
                    if !auto_inject {
                        return Ok(None);
                    }
                    let prompt = match &event.payload {
                        EventPayload::BeforeAgentStart { prompt, .. } => prompt.clone(),
                        _ => return Ok(None),
                    };
                    let (count, _) = store.status().unwrap_or((0, None));
                    if count == 0 || prompt.trim().is_empty() {
                        return Ok(None);
                    }
                    let keywords = extract_keywords(&prompt, 6);
                    if keywords.is_empty() {
                        return Ok(None);
                    }
                    let query = keywords.join(" ");
                    let memories = store.recall(&query, recall_limit).unwrap_or_default();
                    if memories.is_empty() {
                        return Ok(None);
                    }
                    let mut block = "## Recalled memories\n".to_owned();
                    for m in &memories {
                        let snippet: String = m.body.chars().take(200).collect();
                        block.push_str(&format!("- **{}** ({}): {}\n", m.title, m.kind, snippet));
                    }
                    block.push_str("\n_Use memory_recall / memory_explore / memory_reflect to query,                                      walk, and reinforce this graph._");
                    Ok(Some(EventReturn::SystemPromptMod { append: Some(block), prepend: None }))
                }
            }),
        })?;

        // SessionStart: run decay over the vault and notify.
        host.subscribe(EventHandlerRegistration {
            event: EventKind::SessionStart,
            label: "builtin.memory.session-start".to_owned(),
            handler: Arc::new({
                let store = Arc::clone(&store);
                move |_event: phi_ext::LifecycleEvent| {
                    let pruned = store.decay_all().unwrap_or(0);
                    let (count, _) = store.status().unwrap_or((0, None));
                    let msg = if pruned > 0 {
                        format!("[memory] vault ready -- {count} memories ({pruned} decayed)")
                    } else {
                        format!("[memory] vault ready -- {count} memories")
                    };
                    // Push via stderr; the runtime/TUI can surface this.
                    eprintln!("{msg}");
                    Ok(None)
                }
            }),
        })?;

        Ok(())
    }
}

// -- Formatting helpers -----------------------------------------------------

fn format_memories(memories: &[Memory]) -> String {
    if memories.is_empty() {
        return "No memories found.".to_owned();
    }
    memories.iter().map(|m| {
        let snippet: String = m.body.chars().take(300).collect();
        let dots = if m.body.len() > 300 { "..." } else { "" };
        format!("[#{}] {} ({}) [s={:.2} a={:.2}]\n{}{}",
                m.id, m.title, m.kind, m.strength, m.activation, snippet, dots)
    }).collect::<Vec<_>>().join("\n\n")
}

fn format_explored(memories: &[Memory]) -> String {
    if memories.is_empty() {
        return "No related memories discovered.".to_owned();
    }
    let mut out = "## Graph exploration results\n".to_owned();
    for m in memories {
        let snippet: String = m.body.chars().take(200).collect();
        out.push_str(&format!("- **{}** (#{}, {}): {}\n", m.title, m.id, m.kind, snippet));
    }
    out
}

fn format_edges(edges: &[Edge]) -> String {
    if edges.is_empty() {
        return "No edges.".to_owned();
    }
    edges.iter().map(|e| {
        format!("#{} --[{} w={:.2}]--> #{}", e.source, e.relation, e.weight, e.target)
    }).collect::<Vec<_>>().join("\n")
}

/// Cheap keyword extraction: longest non-stopword tokens from the prompt.
/// This is what makes auto-inject work on real natural-language prompts.
fn extract_keywords(text: &str, limit: usize) -> Vec<String> {
    const STOPWORDS: &[&str] = &[
        "the","a","an","and","or","but","if","then","else","for","of","to","in","on","at",
        "by","with","from","as","is","are","was","were","be","been","being","have","has",
        "had","do","does","did","will","would","could","should","may","might","can","this",
        "that","these","those","i","you","he","she","it","we","they","what","which","who",
        "whom","whose","when","where","why","how","about","into","over","under","again",
        "tell","me","show","give","get","make","use","using","used","my","your","our",
    ];
    let mut scored: Vec<(usize, String)> = Vec::new();
    for word in text.split(|c: char| !c.is_alphanumeric()) {
        let w = word.to_lowercase();
        if w.len() < 3 || STOPWORDS.contains(&w.as_str()) {
            continue;
        }
        scored.push((w.len(), w));
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.into_iter().take(limit).map(|(_, w)| w).collect()
}
