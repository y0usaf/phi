//! Mux mode: a single-process session multiplexer. Each session is an
//! embedded TUI loop running as a tokio task, rendering into an in-memory
//! vt100 screen that the mux composites onto the real terminal.

pub mod clipboard;
pub mod driver;
pub mod grid;
pub mod keys;
pub mod persisted;
pub mod status;
pub mod watcher;
pub mod workspace;

pub mod app;
pub mod command_line;
pub mod ui;

/// Statusline mode-label width ("` NOR `" etc.), shared by the command
/// line and the scene renderer.
pub(crate) const MODE_LABEL_WIDTH: i32 = 5;
