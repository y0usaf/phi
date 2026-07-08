//! Protocol layer — wire mapping per API family (spec: `ref/pi` @
//! `c5582102`, pi v0.79.0), per the locked `phi-ai` structure row:
//! protocols own request shaping + stream-event mapping and nothing
//! else; transport mechanics live in [`crate::transport`], catalog and
//! resolution land with the registry (WS2.4).
//!
//! Provenance:
//! - [`options`] — the runtime half of the spec's `types.ts` options
//!   families (`StreamOptions`, `SimpleStreamOptions`; signals, hooks).
//! - [`simple_options`] ← `providers/simple-options.ts`.
//! - [`transform_messages`] ← `providers/transform-messages.ts`.
//! - [`cloudflare`] ← `providers/cloudflare.ts`.
//! - [`copilot_headers`] ← `providers/github-copilot-headers.ts`.
//! - [`anthropic`] ← `providers/anthropic.ts`.
//!
//! Remaining protocol families (openai-completions, openai-responses,
//! google, bedrock) land here in later WS2.3 slices.

pub mod anthropic;
pub mod cloudflare;
pub mod copilot_headers;
pub mod options;
pub mod simple_options;
pub mod transform_messages;

pub use options::{PayloadHook, ResponseHook, SimpleStreamOptions, StreamOptions};

/// Protocol-layer error. The spec throws `Error(message)` and its stream
/// functions fold every failure into `AssistantMessage.errorMessage`;
/// the message string is the parity surface, so this is a typed wrapper
/// around exactly that string.
#[derive(Clone, Debug, thiserror::Error)]
#[error("{0}")]
pub struct ProtocolError(pub String);
