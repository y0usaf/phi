//! Debug utility: run the usage loader (the same one behind `/usage` and
//! `:usage`) against a sessions root and print the aggregate report.
//!
//! ```sh
//! cargo run -p phi-session --example usage_report_check -- ~/.local/share/phi/sessions
//! ```

fn main() {
    let root = std::env::args()
        .nth(1)
        .expect("usage: usage_report_check <sessions-root>");
    let report = phi_session::usage::load_usage_report_from_path(std::path::Path::new(&root));
    println!(
        "files_scanned={} entries={} skipped_duplicates={}",
        report.files_scanned, report.entries, report.skipped_duplicates
    );
    println!(
        "totals: input={} output={} cache_read={} cache_write={} cost=${:.2}",
        report.totals.input_tokens,
        report.totals.output_tokens,
        report.totals.cache_read_tokens,
        report.totals.cache_creation_tokens,
        report.totals.total_cost
    );
    println!("days={}", report.days.len());
    for day in &report.days {
        println!(
            "  {} in={} out={} cost=${:.2} models={}",
            day.date,
            day.totals.input_tokens,
            day.totals.output_tokens,
            day.totals.total_cost,
            day.model_breakdowns.len()
        );
    }
}
