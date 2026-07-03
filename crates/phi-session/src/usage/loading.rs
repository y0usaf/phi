use serde_json::Value;
use std::collections::BTreeMap;
use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::types::*;
use crate::session;

/// All phi sessions, aggregated — including archived ones. Archiving a
/// session moves its file under `ARCHIVE/` but must not retroactively
/// erase its tokens/cost from the usage report.
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

    let mut loader = UsageLoader::new();

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
        for message in loader.usage_entries_from_file(&file).iter() {
            if !processed.insert(message.dedupe_key.clone()) {
                report.skipped_duplicates = report.skipped_duplicates.saturating_add(1);
                continue;
            }

            for entry in &message.entries {
                report.entries = report.entries.saturating_add(1);
                report.totals.add(&entry.totals);

                let day = by_day.entry(entry.date.clone()).or_default();
                day.totals.add(&entry.totals);
                // Skip the `__total__` sentinel from per-model breakdowns:
                // it represents the combined total across all models (written
                // by older builds), not a real model. Its tokens are still
                // counted toward the day and report totals above.
                if entry.model != TOTAL_SENTINEL {
                    day.models
                        .entry(entry.model.clone())
                        .or_default()
                        .add(&entry.totals);
                }
            }
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

/// Deduplicated usage totals for a single session file (e.g. an archived
/// session shown in the archive overlay).
pub fn session_usage_totals(path: &Path) -> UsageTotals {
    let mut processed = HashSet::new();
    let mut totals = UsageTotals::default();
    for message in parse_usage_entries_from_file(path) {
        if !processed.insert(message.dedupe_key.clone()) {
            continue;
        }
        for entry in &message.entries {
            totals.add(&entry.totals);
        }
    }
    totals
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

impl UsageLoader {
    pub(super) fn usage_entries_from_file(&mut self, path: &Path) -> Arc<[MessageUsage]> {
        let cache_key = file_cache_key(path);
        if let Some(cache_key) = cache_key.as_ref()
            && let Some(cached) = self
                .cache
                .get(path)
                .filter(|cached| cached.key == *cache_key)
                .cloned()
        {
            return cached.entries;
        }

        let entries: Arc<[MessageUsage]> = parse_usage_entries_from_file(path).into();
        if let Some(cache_key) = cache_key {
            self.cache.insert(
                path.to_path_buf(),
                CachedUsageEntries {
                    key: cache_key,
                    entries: entries.clone(),
                },
            );
        }
        entries
    }
}

pub(super) fn parse_usage_entries_from_file(path: &Path) -> Vec<MessageUsage> {
    let Ok(file) = fs::File::open(path) else {
        return Vec::new();
    };
    let reader = BufReader::new(file);
    let mut messages = Vec::new();

    for line in reader.lines().map_while(Result::ok) {
        let line = line.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }
        // A multi-process append race can glue two records onto one line
        // (`{..}{..}`); stream-deserialize so every whole object on the
        // line is still counted, stopping at the first malformed one.
        let mut values = serde_json::Deserializer::from_str(line).into_iter::<Value>();
        while let Some(Ok(value)) = values.next() {
            messages.extend(usage_entries_from_json(
                &value,
                &jiff::tz::TimeZone::system(),
            ));
        }
    }

    messages
}

pub(crate) fn usage_entries_from_json(
    value: &Value,
    time_zone: &jiff::tz::TimeZone,
) -> Option<MessageUsage> {
    if value.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    if value.get("role").and_then(Value::as_str) != Some("assistant") {
        return None;
    }

    let timestamp = value
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|raw| raw.parse::<jiff::Timestamp>().ok());
    // Entries from files that predate the timestamp field still carry real
    // tokens/cost; bucket them under a sentinel date instead of dropping
    // them so totals stay complete. The sentinel sorts after `YYYY-MM-DD`
    // dates in the descending day report.
    let date = match timestamp {
        Some(timestamp) => timestamp.to_zoned(time_zone.clone()).date().to_string(),
        None => UNDATED_LABEL.to_owned(),
    };

    let raw_breakdowns = value.get("usage_by_model").and_then(Value::as_array);
    let usage_by_model = raw_breakdowns
        .into_iter()
        .flatten()
        .filter_map(|usage| usage_entry_from_json_usage(timestamp, &date, usage))
        .collect::<Vec<_>>();

    // Use the breakdown only when every element parsed: a partial parse
    // would silently under-count the turn, so fall back to the combined
    // `usage` total instead.
    let entries =
        if raw_breakdowns.is_some_and(|raw| !raw.is_empty() && raw.len() == usage_by_model.len()) {
            usage_by_model
        } else {
            value
                .get("usage")
                .and_then(|usage| usage_entry_from_json_usage(timestamp, &date, usage))
                .into_iter()
                .collect()
        };
    if entries.is_empty() {
        return None;
    }

    // Message-level dedupe key: the message id when present (duplicate ids
    // across files are fork/archive copies of the same turn, whatever
    // representation each copy parses into), else a content fingerprint of
    // the first row.
    let dedupe_key = match value.get("id").and_then(Value::as_str) {
        Some(id) => id.to_owned(),
        None => {
            let first = &entries[0];
            format!(
                "{}\0{}\0{}\0{}\0{}\0{}",
                first.timestamp_ms,
                first.model,
                first.totals.input_tokens,
                first.totals.output_tokens,
                first.totals.cache_creation_tokens,
                first.totals.cache_read_tokens
            )
        }
    };

    Some(MessageUsage {
        dedupe_key,
        entries,
    })
}

/// Day-bucket label for entries whose line predates the timestamp field.
pub const UNDATED_LABEL: &str = "(undated)";

/// Model name written by older builds for the combined `usage` total.
/// Filtered from per-model breakdowns so it never appears as a model row.
pub const TOTAL_SENTINEL: &str = "__total__";

pub(super) fn usage_entry_from_json_usage(
    timestamp: Option<jiff::Timestamp>,
    date: &str,
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

    Some(UsageEntry {
        timestamp_ms: timestamp.map(|ts| ts.as_millisecond()).unwrap_or(0),
        date: date.to_owned(),
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
