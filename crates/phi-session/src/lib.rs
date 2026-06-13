//! One interactive phi session, end to end: layered Lua config, the JSONL
//! session store, per-project session construction, the inline TUI loop, and
//! usage aggregation. This layer knows nothing about the mux; `phi-mux`
//! embeds many of these sessions, and the `phi` binary runs exactly one.
//! Keeping the boundary here guarantees a session behaves identically
//! whether or not it runs under the mux.

pub mod config;
pub mod files;
pub mod session;
pub mod session_builder;
pub mod tui;
pub mod usage;
