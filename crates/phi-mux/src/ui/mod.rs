//! Mux compositor UI widgets, built on the `grid` rendering layer.
//! Scene composition, sidebar, viewport, terminal view, and the
//! `:` command-line widget live here. Overlays are extension-registered
//! (`phi-builtins/src/overlays/`) and drawn through `draw_context_impl`.
//! Cell-grid primitives (cell surfaces, color, packed cells, damage,
//! layout, selection, ANSI, theme) are in the `grid` module.

pub mod draw_context_impl;
pub mod palette;
pub mod scene;
pub mod screen_snapshot;
pub mod sidebar;
pub mod statusbar;
pub mod terminal_scene;
pub mod terminal_view;
pub mod visual_context;
