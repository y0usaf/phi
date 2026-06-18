use std::sync::Arc;

use phi_ext::{ToolInvocation, ToolOutput, ToolParam, ToolSpec};

use crate::store::MemoryStore;

pub fn make_reflect_tool(store: &Arc<MemoryStore>) -> ToolSpec {
    let store = Arc::clone(store);
    ToolSpec {
        name: "memory_reflect".to_owned(),
        description: "Reinforce a memory after it proved useful (Q-learning strength update).".to_owned(),
        params: vec![
            ToolParam { name: "id".to_owned(),     ty: "int".to_owned(),   default: None },
            ToolParam { name: "reward".to_owned(), ty: "float".to_owned(), default: Some("0.5".to_owned()) },
        ],
        handler: Arc::new(move |inv: ToolInvocation| {
            let v: serde_json::Value = serde_json::from_str(&inv.input).unwrap_or_default();
            let id     = v["id"].as_i64().unwrap_or(0);
            let reward = v["reward"].as_f64().unwrap_or(0.5);
            if id == 0 {
                return Ok(ToolOutput { content: "memory_reflect: id is required".to_owned() });
            }
            store.reflect(id, reward)?;
            Ok(ToolOutput { content: format!("Reinforced memory #{id} (reward={reward})") })
        }),
    }
}


