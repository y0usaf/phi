use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub params: Vec<ToolParam>,
    #[serde(skip_serializing)]
    pub handler: ToolHandler,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolParam {
    pub name: String,
    pub ty: String,
    pub default: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolInvocation {
    pub input: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolOutput {
    pub content: String,
}

pub type ToolHandler = Arc<dyn Fn(ToolInvocation) -> Result<ToolOutput> + Send + Sync>;
