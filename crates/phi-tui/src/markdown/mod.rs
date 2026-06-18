//! Markdown → ANSI-styled terminal lines with word wrapping.

mod constants;
mod highlight;
mod output;
mod render;
mod tests;

pub use highlight::{highlight_code, highlight_snippet};
pub use output::highlight_output;
pub use render::{highlight_diff_line, render_markdown};
