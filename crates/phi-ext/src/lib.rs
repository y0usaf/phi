pub mod command;
pub mod event;
pub mod manifest;
pub mod tool;
pub mod traits;

pub use command::{CommandHandler, CommandInvocation, CommandOutput, CommandSpec};
pub use event::{
    EventHandler, EventHandlerRegistration, EventKind, EventPayload, EventReturn,
    InputActionKind, LifecycleEvent,
};
pub use manifest::ExtensionManifest;
pub use tool::{ToolHandler, ToolInvocation, ToolOutput, ToolParam, ToolSpec};
pub use traits::{Extension, ExtensionHost};
