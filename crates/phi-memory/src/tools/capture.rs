use std::sync::Arc;

use phi_ext::{ToolInvocation, ToolOutput, ToolParam, ToolSpec};

use crate::store::MemoryStore;

pub fn make_capture_tool(store: &Arc<MemoryStore>) -> ToolSpec {
    let store = Arc::clone(store);
    ToolSpec {
        name: "memory_capture".to_owned(),
        description: "Save an insight/decision/note to memory. [[wiki-links]] auto-link into the graph.".to_owned(),
        params: vec![
            ToolParam { name: "title".to_owned(), ty: "str".to_owned(), default: None },
            ToolParam { name: "body".to_owned(),  ty: "str".to_owned(), default: None },
            ToolParam { name: "kind".to_owned(),  ty: "str".to_owned(), default: Some("\"note\"".to_owned()) },
        ],
        handler: Arc::new(move |inv: ToolInvocation| {
            let v: serde_json::Value = serde_json::from_str(&inv.input).unwrap_or_default();
            let title = v["title"].as_str().unwrap_or("").to_owned();
            let body  = v["body"].as_str().unwrap_or("").to_owned();
            let kind  = v["kind"].as_str().unwrap_or("note").to_owned();
            if title.is_empty() || body.is_empty() {
                return Ok(ToolOutput { content: "memory_capture: title and body are required".to_owned() });
            }
            let id = store.capture(&title, &body, &kind)?;
            Ok(ToolOutput { content: format!("Captured memory #{id}: {title}") })
        }),
    }
}


