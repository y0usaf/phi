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
pub use render::{
    MarkdownBlockKind, MarkdownSpacingMode, highlight_diff_line, render_markdown,
    render_markdown_lazy, spacing_separates_after,
};
