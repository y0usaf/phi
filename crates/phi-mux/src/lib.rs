//! Mux mode: a single-process session multiplexer. Each session is an
//! embedded TUI loop running as a tokio task, rendering into an in-memory
//! vt100 screen that the mux composites onto the real terminal.

pub mod clipboard;
pub mod status;
pub mod persisted;
pub mod driver;
pub mod keys;
pub mod workspace;
pub mod watcher;

pub mod app;
pub mod command_line;
pub mod ui;
