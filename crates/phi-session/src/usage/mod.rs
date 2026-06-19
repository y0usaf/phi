//! Usage aggregation across all phi session files.

mod loading;
mod rendering;
mod types;

#[cfg(test)]
mod tests;

pub use loading::{load_usage_report, load_usage_report_from_path};
pub use rendering::{
    is_usage_separator_line, is_usage_total_line, usage_header_line, usage_overlay_line_count,
    usage_overlay_lines, usage_separator_line, usage_total_line,
};
pub use types::{UsageDay, UsageModelBreakdown, UsageReport, UsageTotals};
