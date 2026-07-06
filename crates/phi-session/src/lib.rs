//! Session persistence and configuration: layered config, the JSONL session
//! store and tree, the session archive, transcript reconstruction, project
//! trust, and usage aggregation. This layer knows nothing about the mux or the TUI;
//! `phi-session-tui` builds interactive sessions on top of it, `phi-mux`
//! embeds many of them, and the `phi` binary runs exactly one. Keeping the
//! boundary here guarantees a session behaves identically whether or not it
//! runs under the mux.

pub mod agent;
pub mod archive;
pub mod config;
pub mod session;
pub mod style;
pub mod transcript;
pub mod trust;
pub mod usage;
