use std::sync::Arc;

use phi_ext::{ToolInvocation, ToolOutput, ToolParam, ToolSpec};

use crate::format::format_explored;
use crate::store::MemoryStore;

pub fn make_explore_tool(store: &Arc<MemoryStore>) -> ToolSpec {
    let store = Arc::clone(store);
    ToolSpec {
        name: "memory_explore".to_owned(),
        description: "Recursively explore the graph neighborhood around BM25 seeds (PPR walk)."
            .to_owned(),
        params: vec![
            ToolParam {
                name: "query".to_owned(),
                ty: "str".to_owned(),
                default: None,
            },
            ToolParam {
                name: "limit".to_owned(),
                ty: "int".to_owned(),
                default: Some("5".to_owned()),
            },
        ],
        handler: Arc::new(move |inv: ToolInvocation| {
            let v: serde_json::Value = serde_json::from_str(&inv.input).unwrap_or_default();
            let query = v["query"].as_str().unwrap_or("").to_owned();
            let limit = v["limit"].as_u64().unwrap_or(5) as usize;
            if query.is_empty() {
                return Ok(ToolOutput {
                    content: "memory_explore: query is required".to_owned(),
                });
            }
            let discovered = store.explore(&query, limit)?;
            if discovered.is_empty() {
                return Ok(ToolOutput {
                    content: "No related memories discovered in the graph.".to_owned(),
                });
            }
            Ok(ToolOutput {
                content: format_explored(&discovered),
            })
        }),
    }
}
