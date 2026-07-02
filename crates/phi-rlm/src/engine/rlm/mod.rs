//! Rlm module: the main engine loop, split into focused submodules.

mod compaction;
mod core;
mod fallback;
mod outcome;
mod sinks;

pub use core::Rlm;
