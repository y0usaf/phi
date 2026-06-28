//! Mux compositor UI widgets, built on the `grid` rendering layer.
//! Scene composition, sidebar, viewport, terminal view, overlays, and the
//! `:` command-line widget live here. Cell-grid primitives (cell surfaces,
//! color, packed cells, damage, layout, selection, ANSI, theme) are in the
//! `grid` module.

pub mod overlays;
pub mod palette;
pub mod scene;
pub mod screen_snapshot;
pub mod sidebar;
pub mod statusbar;
pub mod terminal_scene;
pub mod terminal_view;
