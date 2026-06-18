use std::sync::Arc;

use phi_ext::{
    EventHandlerRegistration, EventKind, EventPayload, EventReturn, LifecycleEvent,
};

use crate::store::MemoryStore;
use crate::format::extract_keywords;

pub fn make_before_agent_start_handler(
    store: &Arc<MemoryStore>,
    recall_limit: usize,
    auto_inject: bool,
) -> EventHandlerRegistration {
    let store = Arc::clone(store);
    EventHandlerRegistration {
        event: EventKind::BeforeAgentStart,
        label: "builtin.memory.inject".to_owned(),
        handler: Arc::new(move |event: LifecycleEvent| {
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
            let mut block = "## Recalled memories
".to_owned();
            for m in &memories {
                let snippet: String = m.body.chars().take(200).collect();
                block.push_str(&format!("- **{}** ({}): {}
", m.title, m.kind, snippet));
            }
            block.push_str("
_Use memory_recall / memory_explore / memory_reflect to query,                                     walk, and reinforce this graph._
");
            Ok(Some(EventReturn::SystemPromptMod { append: Some(block), prepend: None }))
        }),
    }
}

pub fn make_session_start_handler(
    store: &Arc<MemoryStore>,
) -> EventHandlerRegistration {
    let store = Arc::clone(store);
    EventHandlerRegistration {
        event: EventKind::SessionStart,
        label: "builtin.memory.session-start".to_owned(),
        handler: Arc::new(move |_event: LifecycleEvent| {
            let pruned = store.decay_all().unwrap_or(0);
            let (count, _) = store.status().unwrap_or((0, None));
            let msg = if pruned > 0 {
                format!("[memory] vault ready -- {count} memories ({pruned} decayed)")
            } else {
                format!("[memory] vault ready -- {count} memories")
            };
            eprintln!("{msg}");
            Ok(None)
        }),
    }
}


