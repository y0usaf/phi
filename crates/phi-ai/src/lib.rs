// Re-export types and auth from the extracted crates.
pub mod lm;
pub mod types;

pub use lm::{
    ChatMessage, Completion, ContextPayload, DeltaSink, LanguageModel, Role, ThinkingSink,
    ToolCallEvent, ToolCallSink, UsageBreakdown,
};
pub use phi_ai_auth::{
    AuthCredential, AuthError, AuthStorage, LoginCallbacks, LoginMethod, OAuthCredentials,
    OAuthProviderId, PrintCallbacks,
};
pub use types::{
    Api, AssistantContent, AssistantMessage, Auth, Context, Cost, CostRates, Message, Modality,
    Model, ReasoningControl, StopReason, StreamOptions, ThinkingLevel, ToolCall, ToolDefinition,
    ToolResultMessage, Usage, UserMessage, now_ms,
};

pub use registry::{ApiProvider, Registry};
pub use stream::{Accumulator, AssistantEvent, AssistantEventStream, EventSink};

pub mod overflow;
pub mod providers;
pub mod registry;
pub mod stream;
