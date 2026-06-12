//! Terminal UI primitives for phi: inline scrollback renderer (no alt
//! screen), grapheme-aware input editor, markdown → ANSI rendering, spinner.
//! This crate knows nothing about the RLM engine; phi-app wires the two.

pub mod editor;
pub mod fuzzy;
pub mod markdown;
pub mod selector;
pub mod spinner;
pub mod terminal;

pub use crossterm;
pub use editor::{Editor, EditorAction};
pub use fuzzy::{fuzzy_filter, fuzzy_match};
pub use markdown::{highlight_code, highlight_snippet, render_markdown};
pub use selector::{Selector, SelectorItem, SelectorOutcome};
pub use spinner::Spinner;
pub use terminal::{
    LiveCursor, RawModeGuard, Terminal, fill_bg, strip_ansi, visible_width, wrap_ansi,
    wrap_ansi_locate,
};
