use serde_json::Value;
use std::collections::BTreeMap;
use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::types::*;
use crate::session;

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
            if !processed.insert(entry.dedupe_key.clone()) {
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

pub(super) fn collect_jsonl_files(dir: &Path, files: &mut Vec<PathBuf>) {
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

pub(super) fn file_cache_key(path: &Path) -> Option<FileCacheKey> {
    let metadata = fs::metadata(path).ok()?;
    Some(FileCacheKey {
        len: metadata.len(),
        modified: metadata.modified().ok(),
    })
}

pub(super) fn usage_entries_from_file(path: &Path) -> Arc<[UsageEntry]> {
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

pub(super) fn parse_usage_entries_from_file(path: &Path) -> Vec<UsageEntry> {
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
        entries.extend(usage_entries_from_value(
            &value,
            &jiff::tz::TimeZone::system(),
        ));
    }

    entries
}

pub(crate) fn usage_entries_from_value(
    value: &Value,
    time_zone: &jiff::tz::TimeZone,
) -> Vec<UsageEntry> {
    if value.get("type").and_then(Value::as_str) != Some("message") {
        return Vec::new();
    }
    if value.get("role").and_then(Value::as_str) != Some("assistant") {
        return Vec::new();
    }

    let Some(timestamp) = value
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|raw| raw.parse::<jiff::Timestamp>().ok())
    else {
        return Vec::new();
    };
    let date = timestamp.to_zoned(time_zone.clone()).date().to_string();

    let source_id = value.get("id").and_then(Value::as_str);

    let usage_by_model = value
        .get("usage_by_model")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|usage| usage_entry_from_usage(timestamp, &date, source_id, usage))
        .collect::<Vec<_>>();

    if !usage_by_model.is_empty() {
        return usage_by_model;
    }

    value
        .get("usage")
        .and_then(|usage| usage_entry_from_usage(timestamp, &date, source_id, usage))
        .into_iter()
        .collect()
}

pub(super) fn usage_entry_from_usage(
    timestamp: jiff::Timestamp,
    date: &str,
    source_id: Option<&str>,
    usage: &Value,
) -> Option<UsageEntry> {
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

    let dedupe_key = match source_id {
        Some(id) => format!("{id}\0{model}"),
        None => format!(
            "{}\0{}\0{}\0{}\0{}\0{}",
            timestamp.as_millisecond(),
            model,
            input_tokens,
            output_tokens,
            cache_creation_tokens,
            cache_read_tokens
        ),
    };

    Some(UsageEntry {
        timestamp_ms: timestamp.as_millisecond(),
        date: date.to_owned(),
        model,
        totals: UsageTotals {
            input_tokens,
            output_tokens,
            cache_creation_tokens,
            cache_read_tokens,
            total_cost,
        },
        dedupe_key,
    })
}

pub(super) fn number_as_u64(value: &Value) -> Option<u64> {
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
