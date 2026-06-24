//! Terminal UI primitives for phi: inline scrollback renderer (no alt
//! screen), grapheme-aware input editor, markdown → ANSI rendering, spinner.
//! This crate knows nothing about the RLM engine; phi-app wires the two.

pub mod box_render;
pub mod cells;
pub mod editor;
pub mod fuzzy;
pub mod markdown;
pub mod palette;
pub mod selector;
pub mod donut;
pub mod spinner;
pub mod style;
pub mod terminal;

pub use cells::{
    display_cell_width, truncate_line_in_place_to_width, truncate_line_preserving_suffix_to_width,
    truncate_line_to_width, truncate_line_with_ellipsis_to_width, truncate_to_cells,
};
pub use crossterm;
pub use editor::{Editor, EditorAction};
pub use fuzzy::{fuzzy_filter, fuzzy_match};
pub use markdown::{
    highlight_code, highlight_diff_line, highlight_output, highlight_snippet, render_markdown,
    MarkdownPalette,
};
pub use palette::{Rgb, TerminalColors, detect_terminal_colors};
pub use selector::{Selector, SelectorItem, SelectorOutcome};
pub use spinner::Spinner;
pub use terminal::{
    LiveCursor, RawModeGuard, Terminal, fill_bg, strip_ansi, visible_width, wrap_ansi,
    wrap_ansi_locate,
};
