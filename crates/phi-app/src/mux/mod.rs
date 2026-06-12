//! Mux mode: a single-process session multiplexer. Each session is an
//! embedded TUI loop running as a tokio task, rendering into an in-memory
//! vt100 screen that the mux composites onto the real terminal.

pub mod app;
pub mod clipboard;
pub mod command;
pub mod command_line;
pub mod driver;
pub mod keymap;
pub mod persisted;
pub mod status;
pub mod ui;
pub mod usage;
pub mod workspace;
