// All event types have been extracted to the `phi-event` crate.
// This module re-exports them for backward compatibility.

pub use phi_event::{
    EventHandler, EventHandlerRegistration, EventKind, EventPayload, EventReturn, InputActionKind,
    LifecycleEvent,
};
