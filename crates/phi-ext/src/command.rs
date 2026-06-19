use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize)]
pub struct CommandSpec {
    pub name: String,
    pub description: String,
    #[serde(skip_serializing)]
    pub handler: CommandHandler,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommandInvocation {
    pub raw_args: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommandOutput {
    pub message: Option<String>,
}

pub type CommandHandler = Arc<dyn Fn(CommandInvocation) -> Result<CommandOutput> + Send + Sync>;
