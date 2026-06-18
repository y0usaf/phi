use std::sync::Arc;

use phi_ext::{ToolInvocation, ToolOutput, ToolParam, ToolSpec};

use crate::store::MemoryStore;

pub fn make_link_tool(store: &Arc<MemoryStore>) -> ToolSpec {
    let store = Arc::clone(store);
    ToolSpec {
        name: "memory_link".to_owned(),
        description: "Create an explicit typed edge between two memories in the knowledge graph.".to_owned(),
        params: vec![
            ToolParam { name: "source".to_owned(),   ty: "int".to_owned(), default: None },
            ToolParam { name: "target".to_owned(),   ty: "int".to_owned(), default: None },
            ToolParam { name: "relation".to_owned(), ty: "str".to_owned(), default: Some("\"related\"".to_owned()) },
            ToolParam { name: "weight".to_owned(),   ty: "float".to_owned(), default: Some("0.5".to_owned()) },
        ],
        handler: Arc::new(move |inv: ToolInvocation| {
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
        }),
    }
}


