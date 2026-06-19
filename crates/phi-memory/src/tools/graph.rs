use std::sync::Arc;

use phi_ext::{ToolInvocation, ToolOutput, ToolParam, ToolSpec};

use crate::format::format_edges;
use crate::store::MemoryStore;

pub fn make_graph_tool(store: &Arc<MemoryStore>) -> ToolSpec {
    let store = Arc::clone(store);
    ToolSpec {
        name: "memory_graph".to_owned(),
        description: "Show the knowledge-graph edges attached to a memory.".to_owned(),
        params: vec![ToolParam {
            name: "id".to_owned(),
            ty: "int".to_owned(),
            default: None,
        }],
        handler: Arc::new(move |inv: ToolInvocation| {
            let v: serde_json::Value = serde_json::from_str(&inv.input).unwrap_or_default();
            let id = v["id"].as_i64().unwrap_or(0);
            if id == 0 {
                return Ok(ToolOutput {
                    content: "memory_graph: id is required".to_owned(),
                });
            }
            let edges = store.edges_of(id)?;
            Ok(ToolOutput {
                content: format_edges(&edges),
            })
        }),
    }
}
