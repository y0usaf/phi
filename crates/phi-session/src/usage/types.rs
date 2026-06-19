use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;


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

    pub(super) fn add(&mut self, other: &Self) {
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
pub(super) struct UsageEntry {
    pub(super) timestamp_ms: i64,
    pub(super) date: String,
    pub(super) model: String,
    pub(super) totals: UsageTotals,
    pub(super) dedupe_key: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FileCacheKey {
    pub(super) len: u64,
    pub(super) modified: Option<SystemTime>,
}

#[derive(Clone)]
pub(super) struct CachedUsageEntries {
    pub(super) key: FileCacheKey,
    pub(super) entries: Arc<[UsageEntry]>,
}

pub(super) static USAGE_ENTRIES_CACHE: OnceLock<Mutex<HashMap<PathBuf, CachedUsageEntries>>> =
    OnceLock::new();

pub(super) fn usage_entries_cache() -> &'static Mutex<HashMap<PathBuf, CachedUsageEntries>> {
    USAGE_ENTRIES_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Default)]
pub(super) struct DayAccumulator {
    pub(super) totals: UsageTotals,
    pub(super) models: BTreeMap<String, UsageTotals>,
}
