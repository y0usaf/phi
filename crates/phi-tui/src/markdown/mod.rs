//! Markdown → ANSI-styled terminal lines with word wrapping.

mod constants;
mod highlight;
mod incremental;
mod balanced_wrap;
mod checkpoint;
mod output;
mod render;

#[cfg(test)]
mod tests;

pub use constants::MarkdownPalette;
pub use highlight::{highlight_code, highlight_code_cached, highlight_snippet};
pub use incremental::IncrementalMarkdownRenderer;
pub use balanced_wrap::wrap_line_balanced;
pub use output::highlight_output;
pub use render::{highlight_diff_line, render_markdown, render_markdown_lazy, center_structured_block_ranges, apply_centering, StructuredBlockType, MarkdownSpacingMode, spacing_separates_after};
pub use checkpoint::{find_last_complete_block, find_last_complete_block_checkpoint, CompleteBlockCheckpoint, MarkdownBlockKind};
