//! Terminal UI primitives for phi: inline scrollback renderer (no alt
//! screen), grapheme-aware input editor, markdown → ANSI rendering, spinner.
//! This crate knows nothing about the RLM engine; phi-app wires the two.

pub mod cells;
pub mod editor;
pub mod fuzzy;
#[cfg(feature = "markdown")]
pub mod markdown;
pub mod palette;
pub mod selector;
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
#[cfg(feature = "markdown")]
pub use markdown::{
    MarkdownPalette, highlight_code, highlight_diff_line, highlight_output, render_markdown,
};
pub use palette::{Rgb, TerminalColors, detect_terminal_colors};
pub use selector::{Selector, SelectorItem, SelectorOutcome};
pub use spinner::{
    DEFAULT_TOOL_EXPRESSION, EQUALIZER_BARS, EQUALIZER_BAR_COUNT, SPINNER_FRAMES,
    SPINNER_FRAME_MS, Spinner, THINKING_EXPRESSIONS, THINKING_EXPRESSION_MS,
    equalizer_glyphs, gradient_text, spinner_frame_at, spinner_frame_index, thinking_expression,
};
pub use terminal::{
    LiveCursor, RawModeGuard, Terminal, fill_bg, strip_ansi, visible_width, wrap_ansi,
    wrap_ansi_locate,
};
