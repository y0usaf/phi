//! One interactive phi session, end to end: layered config, the JSONL
//! session store, per-project session construction, the inline TUI loop, and
//! usage aggregation. This layer knows nothing about the mux; `phi-mux`
//! embeds many of these sessions, and the `phi` binary runs exactly one.
//! Keeping the boundary here guarantees a session behaves identically
//! whether or not it runs under the mux.

pub mod config;
pub mod files;
pub mod prompts;
pub mod session;
pub mod skills;
pub mod tool_summary;
pub mod transcript;
pub mod usage;
