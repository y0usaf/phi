//! Mux compositor UI widgets, built on the phi-mux-grid rendering layer.
//! Scene composition, sidebar, viewport, terminal view, overlays, and the
//! `:` command-line widget live here. Cell-grid primitives (cell surfaces,
//! color, packed cells, damage, layout, selection, ANSI, theme) are in the
//! `phi-mux-grid` crate.

pub mod scene;
pub mod sidebar;
pub mod terminal_view;
pub mod viewport;
pub mod overlays;
