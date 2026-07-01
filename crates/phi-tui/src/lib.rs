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
pub mod theme;

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
    DEFAULT_TOOL_EXPRESSION, EQUALIZER_BAR_COUNT, EQUALIZER_BARS, SHIMMER_BLEND_PEAK,
    SHIMMER_CYCLE_MS, SHIMMER_FRAME_MS, SHIMMER_MIN_WINDOW, SHIMMER_TRIANGLE_BLEND,
    SHIMMER_TRIANGLE_STEPS, SHIMMER_WINDOW_FRAC, SPINNER_FRAME_MS, SPINNER_FRAMES, Spinner,
    THINKING_ANIMATION_FRAME_MS, THINKING_EXPRESSION_MS, THINKING_EXPRESSIONS, equalizer_glyphs,
    gradient_text, gradient_text_animated, shimmer_ansi, shimmer_triangle_blend,
    shimmer_triangle_intensity, spinner_frame_at, spinner_frame_index, thinking_expression,
};
pub use terminal::{
    LiveCursor, RawModeGuard, Terminal, drain_input, escape_len, fill_bg, strip_ansi,
    visible_width, wrap_ansi, wrap_ansi_locate,
};
pub use theme::{
    Mode, Theme, ansi_index_to_rgb, bg_escape, bold_fg_escape, brighten, default_terminal_colors,
    fade_toward, fg_escape, generate_theme, luminance_rgb, terminal_mode,
};
