//! Mux mode: a single-process session multiplexer. Each session is an
//! embedded TUI loop running as a tokio task, rendering into an in-memory
//! vt100 screen that the mux composites onto the real terminal.

// Re-exports from extracted leaf crates for backward compatibility.
pub use phi_mux_clipboard as clipboard;
pub use phi_mux_keys as keymap;
pub use phi_mux_persisted as persisted;
pub use phi_mux_status as status;
pub use phi_mux_driver as driver;
pub use phi_mux_workspace as workspace;

pub mod app;
pub mod command_line;
pub mod ui;
