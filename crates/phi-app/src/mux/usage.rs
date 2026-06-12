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
}
