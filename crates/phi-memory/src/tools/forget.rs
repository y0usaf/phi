use std::sync::Arc;

use phi_ext::{ToolInvocation, ToolOutput, ToolParam, ToolSpec};

use crate::store::MemoryStore;

pub fn make_forget_tool(store: &Arc<MemoryStore>) -> ToolSpec {
    let store = Arc::clone(store);
    ToolSpec {
        name: "memory_forget".to_owned(),
        description: "Permanently delete a memory and its edges from the vault.".to_owned(),
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
                    content: "memory_forget: id is required".to_owned(),
                });
            }
            store.forget(id)?;
            Ok(ToolOutput {
                content: format!("Forgot memory #{id}"),
            })
        }),
    }
}
