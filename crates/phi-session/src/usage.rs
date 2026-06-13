//! Usage aggregation across all phi session files (port of pi-harness
//! `pi::usage` on phi's JSONL shape): assistant message entries carrying a
//! `usage` field, bucketed day×model in local time, deduped by
//! `(timestamp, total tokens)`, with a per-file cache keyed on (len, mtime).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use phi_tui::{display_cell_width, truncate_to_cells};
use serde_json::Value;

use crate::session;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct UsageTotals {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub total_cost: f64,
}

impl UsageTotals {
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.output_tokens)
            .saturating_add(self.cache_creation_tokens)
            .saturating_add(self.cache_read_tokens)
    }

    fn add(&mut self, other: &Self) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.cache_creation_tokens = self
            .cache_creation_tokens
            .saturating_add(other.cache_creation_tokens);
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(other.cache_read_tokens);
        self.total_cost += other.total_cost;
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct UsageModelBreakdown {
    pub model_name: String,
    pub totals: UsageTotals,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UsageDay {
    pub date: String,
    pub totals: UsageTotals,
    pub model_breakdowns: Vec<UsageModelBreakdown>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct UsageReport {
    pub days: Vec<UsageDay>,
    pub totals: UsageTotals,
    pub files_scanned: usize,
    pub entries: usize,
    pub skipped_duplicates: usize,
}

#[derive(Clone, Debug, PartialEq)]
struct UsageEntry {
    timestamp_ms: i64,
    date: String,
    model: String,
    totals: UsageTotals,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileCacheKey {
    len: u64,
    modified: Option<SystemTime>,
}

#[derive(Clone)]
struct CachedUsageEntries {
    key: FileCacheKey,
    entries: Arc<[UsageEntry]>,
}

static USAGE_ENTRIES_CACHE: OnceLock<Mutex<HashMap<PathBuf, CachedUsageEntries>>> = OnceLock::new();

fn usage_entries_cache() -> &'static Mutex<HashMap<PathBuf, CachedUsageEntries>> {
    USAGE_ENTRIES_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Default)]
struct DayAccumulator {
    totals: UsageTotals,
    models: BTreeMap<String, UsageTotals>,
}

/// All non-archived phi sessions, aggregated.
pub fn load_usage_report() -> UsageReport {
    match session::sessions_root() {
        Ok(root) => load_usage_report_from_path(&root),
        Err(_) => UsageReport::default(),
    }
}

pub fn load_usage_report_from_path(root: &Path) -> UsageReport {
    if !root.is_dir() {
        return UsageReport::default();
    }

    let mut files = Vec::new();
    collect_jsonl_files(root, &mut files);
    files.sort();

    let mut report = UsageReport {
        files_scanned: files.len(),
        ..UsageReport::default()
    };
    let mut processed = HashSet::new();
    let mut by_day = BTreeMap::<String, DayAccumulator>::new();

    for file in files {
        for entry in usage_entries_from_file(&file).iter() {
            if !processed.insert((entry.timestamp_ms, entry.totals.total_tokens())) {
                report.skipped_duplicates = report.skipped_duplicates.saturating_add(1);
                continue;
            }

            report.entries = report.entries.saturating_add(1);
            report.totals.add(&entry.totals);

            let day = by_day.entry(entry.date.clone()).or_default();
            day.totals.add(&entry.totals);
            day.models
                .entry(entry.model.clone())
                .or_default()
                .add(&entry.totals);
        }
    }

    report.days = by_day
        .into_iter()
        .map(|(date, day)| UsageDay {
            date,
            totals: day.totals,
            model_breakdowns: day
                .models
                .into_iter()
                .map(|(model_name, totals)| UsageModelBreakdown { model_name, totals })
                .collect(),
        })
        .collect();
    report.days.sort_by(|a, b| b.date.cmp(&a.date));

    report
}

fn collect_jsonl_files(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            // Archived sessions don't count toward usage.
            if path.file_name().is_some_and(|name| name == "ARCHIVE") {
                continue;
            }
            collect_jsonl_files(&path, files);
        } else if file_type.is_file()
            && path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
        {
            files.push(path);
        }
    }
}

fn file_cache_key(path: &Path) -> Option<FileCacheKey> {
    let metadata = fs::metadata(path).ok()?;
    Some(FileCacheKey {
        len: metadata.len(),
        modified: metadata.modified().ok(),
    })
}

fn usage_entries_from_file(path: &Path) -> Arc<[UsageEntry]> {
    let cache_key = file_cache_key(path);
    if let Some(cache_key) = cache_key.as_ref()
        && let Some(cached) = usage_entries_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(path)
            .filter(|cached| cached.key == *cache_key)
            .cloned()
    {
        return cached.entries;
    }

    let entries: Arc<[UsageEntry]> = parse_usage_entries_from_file(path).into();
    if let Some(cache_key) = cache_key {
        usage_entries_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                path.to_path_buf(),
                CachedUsageEntries {
                    key: cache_key,
                    entries: entries.clone(),
                },
            );
    }
    entries
}

fn parse_usage_entries_from_file(path: &Path) -> Vec<UsageEntry> {
    let Ok(file) = fs::File::open(path) else {
        return Vec::new();
    };
    let reader = BufReader::new(file);
    let mut entries = Vec::new();

    for line in reader.lines().map_while(Result::ok) {
        let line = line.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(entry) = usage_entry_from_value(&value, &jiff::tz::TimeZone::system()) {
            entries.push(entry);
        }
    }

    entries
}

fn usage_entry_from_value(value: &Value, time_zone: &jiff::tz::TimeZone) -> Option<UsageEntry> {
    if value.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    if value.get("role").and_then(Value::as_str) != Some("assistant") {
        return None;
    }

    let timestamp: jiff::Timestamp = value.get("timestamp")?.as_str()?.parse().ok()?;
    let date = timestamp.to_zoned(time_zone.clone()).date().to_string();

    let usage = value.get("usage")?;
    let input_tokens = number_as_u64(usage.get("input")?)?;
    let output_tokens = number_as_u64(usage.get("output")?)?;
    let cache_read_tokens = usage.get("cache_read").and_then(number_as_u64).unwrap_or(0);
    let cache_creation_tokens = usage
        .get("cache_write")
        .and_then(number_as_u64)
        .unwrap_or(0);
    let total_cost = usage
        .get("cost_total")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let model = usage
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();

    Some(UsageEntry {
        timestamp_ms: timestamp.as_millisecond(),
        date,
        model,
        totals: UsageTotals {
            input_tokens,
            output_tokens,
            cache_creation_tokens,
            cache_read_tokens,
            total_cost,
        },
    })
}

fn number_as_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|number| u64::try_from(number).ok()))
        .or_else(|| {
            value.as_f64().and_then(|number| {
                (number.is_finite()
                    && number >= 0.0
                    && number <= u64::MAX as f64
                    && number.fract() == 0.0)
                    .then_some(number as u64)
            })
        })
}


// --- Usage table lines: pure text formatting shared by the inline TUI's
// /usage scrollback dump and the mux's usage overlay, so both modes render
// identical tables. ---

const USAGE_TREE_COLS: usize = 40;
const USAGE_VALUE_COLS: [usize; 6] = [10, 10, 10, 10, 10, 9];

pub fn usage_overlay_line_count(report: &UsageReport) -> usize {
    let data_rows = report
        .days
        .iter()
        .map(|day| 1 + day.model_breakdowns.len())
        .sum::<usize>();
    data_rows.saturating_add(report.days.len().saturating_sub(1))
}

/// Scrollable day/model rows; the total row stays fixed in the overlay.
pub fn usage_overlay_lines(report: &UsageReport) -> Vec<String> {
    let mut lines = Vec::with_capacity(usage_overlay_line_count(report));
    for (day_index, day) in report.days.iter().enumerate() {
        lines.push(usage_day_line(day));
        let models = sorted_model_breakdowns(day);
        for (index, breakdown) in models.iter().enumerate() {
            lines.push(usage_model_line(breakdown, index + 1 == models.len()));
        }
        if day_index + 1 < report.days.len() {
            lines.push(usage_separator_line());
        }
    }
    lines
}


pub fn usage_header_line() -> String {
    usage_table_line(
        "DATE / MODEL",
        ["INPUT", "OUTPUT", "CACHE+", "CACHE↺", "TOTAL", "COST"],
    )
}

pub fn usage_separator_line() -> String {
    let mut line = "─".repeat(USAGE_TREE_COLS);
    for width in USAGE_VALUE_COLS {
        line.push('┼');
        line.push_str(&"─".repeat(width));
    }
    line
}

pub fn is_usage_separator_line(line: &str) -> bool {
    line.starts_with('─')
}

pub fn is_usage_total_line(line: &str) -> bool {
    !is_usage_separator_line(line) && !line.starts_with("  ├─ ") && !line.starts_with("  └─ ")
}

pub fn usage_total_line(totals: &UsageTotals) -> String {
    usage_row_line("TOTAL", totals)
}

fn usage_day_line(day: &UsageDay) -> String {
    usage_row_line(&day.date, &day.totals)
}

fn usage_model_line(breakdown: &UsageModelBreakdown, is_last: bool) -> String {
    let branch = if is_last { "  └─ " } else { "  ├─ " };
    let label = format!("{branch}{}", format_model_name(&breakdown.model_name));
    usage_row_line(&label, &breakdown.totals)
}

fn sorted_model_breakdowns(day: &UsageDay) -> Vec<UsageModelBreakdown> {
    let mut models = day.model_breakdowns.clone();
    models.sort_by_key(|a| format_model_name(&a.model_name));
    models
}

fn usage_row_line(label: &str, totals: &UsageTotals) -> String {
    let input = format_u64(totals.input_tokens);
    let output = format_u64(totals.output_tokens);
    let cache_creation = format_u64(totals.cache_creation_tokens);
    let cache_read = format_u64(totals.cache_read_tokens);
    let total = format_u64(totals.total_tokens());
    let cost = format_currency(totals.total_cost);
    usage_table_line(
        label,
        [&input, &output, &cache_creation, &cache_read, &total, &cost],
    )
}

fn usage_table_line(label: &str, values: [&str; 6]) -> String {
    let mut line = format_cell_left(label, USAGE_TREE_COLS);
    for (value, width) in values.iter().zip(USAGE_VALUE_COLS.iter()) {
        line.push('│');
        line.push_str(&format_cell_right(value, *width));
    }
    line
}

fn format_cell_left(value: &str, width: usize) -> String {
    let value = truncate_to_cells(value, width);
    let padding = width.saturating_sub(display_cell_width(&value));
    format!("{value}{}", " ".repeat(padding))
}

fn format_cell_right(value: &str, width: usize) -> String {
    let value = truncate_to_cells(value, width);
    let padding = width.saturating_sub(display_cell_width(&value));
    format!("{}{value}", " ".repeat(padding))
}

fn format_model_name(model: &str) -> String {
    if let Some(rest) = model.strip_prefix("anthropic/") {
        return format_model_name(rest);
    }

    let Some(rest) = model.strip_prefix("claude-") else {
        return model.to_string();
    };

    let mut parts = rest.split('-').collect::<Vec<_>>();
    if parts
        .last()
        .is_some_and(|part| part.len() == 8 && part.chars().all(|ch| ch.is_ascii_digit()))
    {
        parts.pop();
    }
    parts.join("-")
}

fn format_u64(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

fn format_currency(amount: f64) -> String {
    format!("${amount:.2}")
}


#[cfg(test)]
mod tests {
    use super::*;

    fn message(timestamp: &str, model: &str, input: u64, output: u64, cost: f64) -> String {
        format!(
            r#"{{"type":"message","id":"x","parent_id":null,"timestamp":"{timestamp}","role":"assistant","text":"hi","usage":{{"input":{input},"output":{output},"cache_read":10,"cache_write":20,"cost_total":{cost},"model":"{model}"}}}}"#
        )
    }

    #[test]
    fn usage_entry_parses_phi_session_shape() {
        let value: Value = serde_json::from_str(&message(
            "2026-01-01T00:00:01Z",
            "claude-opus-4-5",
            100,
            50,
            0.05,
        ))
        .unwrap();
        let entry = usage_entry_from_value(&value, &jiff::tz::TimeZone::UTC).unwrap();
        assert_eq!(entry.date, "2026-01-01");
        assert_eq!(entry.model, "claude-opus-4-5");
        assert_eq!(entry.totals.input_tokens, 100);
        assert_eq!(entry.totals.cache_read_tokens, 10);
        assert_eq!(entry.totals.cache_creation_tokens, 20);
        assert_eq!(entry.totals.total_cost, 0.05);
    }

    #[test]
    fn usage_entry_rejects_user_and_usage_less_entries() {
        let user: Value = serde_json::from_str(
            r#"{"type":"message","id":"x","timestamp":"2026-01-01T00:00:01Z","role":"user","text":"hi"}"#,
        )
        .unwrap();
        assert!(usage_entry_from_value(&user, &jiff::tz::TimeZone::UTC).is_none());
        let no_usage: Value = serde_json::from_str(
            r#"{"type":"message","id":"x","timestamp":"2026-01-01T00:00:01Z","role":"assistant","text":"hi"}"#,
        )
        .unwrap();
        assert!(usage_entry_from_value(&no_usage, &jiff::tz::TimeZone::UTC).is_none());
    }

    #[test]
    fn report_aggregates_by_day_and_model_skipping_archive_and_duplicates() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("--tmp-project--");
        fs::create_dir_all(&project).unwrap();
        let lines = [
            r#"{"type":"session","id":"s1","timestamp":"2026-01-01T00:00:00Z","cwd":"/tmp/project"}"#.to_owned(),
            message("2026-01-03T12:00:00Z", "claude-sonnet-4", 100, 50, 0.05),
            message("2026-01-03T12:00:00Z", "claude-sonnet-4", 100, 50, 0.05),
            message("2026-01-01T12:00:00Z", "claude-opus-4-5", 7, 8, 0.01),
        ];
        fs::write(project.join("a.jsonl"), lines.join("\n")).unwrap();
        let archive = tmp.path().join("ARCHIVE");
        fs::create_dir_all(&archive).unwrap();
        fs::write(
            archive.join("old.jsonl"),
            message("2026-01-02T12:00:00Z", "claude-opus-4-5", 999, 999, 9.0),
        )
        .unwrap();

        let report = load_usage_report_from_path(tmp.path());

        assert_eq!(report.files_scanned, 1);
        assert_eq!(report.entries, 2);
        assert_eq!(report.skipped_duplicates, 1);
        assert_eq!(report.totals.input_tokens, 107);
        assert_eq!(report.days.len(), 2);
        assert!(report.days[0].date > report.days[1].date);
        assert_eq!(
            report.days[0].model_breakdowns[0].model_name,
            "claude-sonnet-4"
        );
    }

    use super::*;

    #[test]
    fn formats_model_names_like_pi_usage() {
        assert_eq!(format_model_name("claude-opus-4-5"), "opus-4-5");
        assert_eq!(format_model_name("claude-sonnet-4-20250514"), "sonnet-4");
        assert_eq!(format_model_name("anthropic/claude-opus-4.5"), "opus-4.5");
    }

    #[test]
    fn usage_rows_include_totals_and_cost() {
        let report = UsageReport {
            days: vec![UsageDay {
                date: "2026-01-02".into(),
                totals: UsageTotals {
                    input_tokens: 10,
                    output_tokens: 12,
                    cache_creation_tokens: 1,
                    cache_read_tokens: 2,
                    total_cost: 0.03,
                },
                model_breakdowns: vec![
                    UsageModelBreakdown {
                        model_name: "claude-sonnet-4".into(),
                        totals: UsageTotals {
                            input_tokens: 7,
                            output_tokens: 8,
                            cache_creation_tokens: 1,
                            cache_read_tokens: 2,
                            total_cost: 0.01,
                        },
                    },
                    UsageModelBreakdown {
                        model_name: "claude-opus-4-5".into(),
                        totals: UsageTotals {
                            input_tokens: 3,
                            output_tokens: 4,
                            cache_creation_tokens: 0,
                            cache_read_tokens: 0,
                            total_cost: 0.02,
                        },
                    },
                ],
            }],
            totals: UsageTotals {
                input_tokens: 1000,
                output_tokens: 50,
                cache_creation_tokens: 20,
                cache_read_tokens: 10,
                total_cost: 0.055,
            },
            files_scanned: 1,
            entries: 1,
            skipped_duplicates: 0,
        };

        let daily_rows = usage_overlay_lines(&report);
        let header_row = usage_header_line();
        let separator_row = usage_separator_line();

        assert_eq!(
            display_cell_width(&header_row),
            display_cell_width(&separator_row)
        );
        assert!(header_row.contains("DATE / MODEL"));
        assert!(header_row.contains('│'));
        assert!(separator_row.contains('┼'));
        assert_eq!(daily_rows.len(), 3);
        assert!(daily_rows[0].contains("2026-01-02"));
        assert!(daily_rows[0].contains('│'));
        assert!(daily_rows[0].contains("25"));
        assert!(daily_rows[0].contains("$0.03"));
        assert!(daily_rows[1].contains("├─ opus-4-5"));
        assert!(daily_rows[1].contains("$0.02"));
        assert!(daily_rows[2].contains("└─ sonnet-4"));
        assert!(daily_rows[2].contains("$0.01"));

        let total_row = usage_total_line(&report.totals);

        assert!(total_row.contains("TOTAL"));
        assert!(total_row.contains("1,080"));
        assert!(total_row.contains("$0.06"));
    }

    #[test]
    fn usage_lines_separate_day_groups() {
        let day = |date: &str| UsageDay {
            date: date.into(),
            totals: UsageTotals {
                input_tokens: 1,
                output_tokens: 2,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
                total_cost: 0.01,
            },
            model_breakdowns: Vec::new(),
        };
        let report = UsageReport {
            days: vec![day("2026-01-02"), day("2026-01-01")],
            totals: UsageTotals::default(),
            files_scanned: 1,
            entries: 2,
            skipped_duplicates: 0,
        };

        let rows = usage_overlay_lines(&report);

        assert_eq!(usage_overlay_line_count(&report), 3);
        assert_eq!(rows.len(), 3);
        assert!(rows[1].starts_with('─'));
        assert!(rows[1].contains('┼'));
    }
}
