use std::sync::Arc;

use phi_ext::{ToolInvocation, ToolOutput, ToolParam, ToolSpec};

use crate::store::MemoryStore;
use crate::format::format_memories;

pub fn make_recall_tool(store: &Arc<MemoryStore>) -> ToolSpec {
    let store = Arc::clone(store);
    ToolSpec {
        name: "memory_recall".to_owned(),
        description: "Search your persistent memory vault with hybrid BM25+graph+decay fusion.".to_owned(),
        params: vec![
            ToolParam { name: "query".to_owned(), ty: "str".to_owned(), default: None },
            ToolParam { name: "limit".to_owned(), ty: "int".to_owned(), default: Some("5".to_owned()) },
        ],
        handler: Arc::new(move |inv: ToolInvocation| {
            let v: serde_json::Value = serde_json::from_str(&inv.input).unwrap_or_default();
            let query = v["query"].as_str().unwrap_or("").to_owned();
            let limit = v["limit"].as_u64().unwrap_or(5) as usize;
            if query.is_empty() {
                return Ok(ToolOutput { content: "memory_recall: query is required".to_owned() });
            }
            let memories = store.recall(&query, limit)?;
            Ok(ToolOutput { content: format_memories(&memories) })
        }),
    }
}


