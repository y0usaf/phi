//! Markdown → ANSI-styled terminal lines with word wrapping.

mod constants;
mod highlight;
mod output;
mod render;

#[cfg(test)]
mod tests;

pub use constants::MarkdownPalette;
pub use highlight::highlight_code;
pub use output::highlight_output;
pub use render::{highlight_diff_line, render_markdown};
