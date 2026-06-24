//! Mux compositor UI, ported from pi-harness: a cell surface diffed to the
//! real terminal, with sidebar, embedded terminal view, statusline, and
//! overlays drawn on top.

pub mod ansi;
pub mod anchor_stability;
pub mod catchup;
pub mod cell_surface;
pub mod color;
pub mod damage;
pub mod message_cache;
pub mod layout;
pub mod overlays;
pub mod prefix_reuse;
pub mod packed;
pub mod prompt_anim;
pub mod prepared_frame;
pub mod scene;
pub mod selection;
pub mod sidebar;
pub mod terminal_view;
pub mod viewport;
pub mod theme;
