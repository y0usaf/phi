//! Mux compositor UI, ported from pi-harness: a cell surface diffed to the
//! real terminal, with sidebar, embedded terminal view, statusline, and
//! overlays drawn on top.

pub mod ansi;
pub mod cell_surface;
pub mod color;
pub mod layout;
pub mod overlays;
pub mod scene;
pub mod selection;
pub mod sidebar;
pub mod terminal_view;
pub mod theme;
