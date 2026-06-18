//! Usage aggregation across all phi session files.

mod loading;
mod rendering;
mod tests;
mod types;

pub use types::{UsageDay, UsageModelBreakdown, UsageReport, UsageTotals};
pub use loading::{load_usage_report, load_usage_report_from_path};
pub use rendering::{
    usage_overlay_line_count, usage_overlay_lines, usage_header_line,
    usage_separator_line, is_usage_separator_line, is_usage_total_line,
    usage_total_line,
};
