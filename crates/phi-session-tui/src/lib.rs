//! TUI session loop and per-project session construction for phi.
//!
//! This crate is the heavy-TUI companion to `phi-session`: it contains the
//! inline terminal UI loop (`tui`) and the session builder that wires together
//! the RLM engine, runtime, extensions, and model providers. `phi-session`
//! itself stays lightweight (config, transcript, usage, session store) so
//! that downstream crates can use those types without pulling in tokio, mlua,
//! or the full provider stack.

pub mod session_builder;
pub mod tui;
