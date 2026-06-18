use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct BlockSpec<'a> {
    pub key: &'a str,
    pub lines: &'a [String],
    pub finalized: bool,
    pub commit_stable: bool,
    pub version: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Frame {
    pub scrollback: Vec<String>,
    pub live: Vec<String>,
    pub full: Vec<String>,
    pub live_region_start: Option<usize>,
    pub commit_safe_end: Option<usize>,
    pub snapshot_safe_end: Option<usize>,
    pub committed_rows: usize,
}

#[derive(Clone, Debug, Default)]
pub(super) struct LiveDiffSnapshot {
    pub(super) width: usize,
    pub(super) lines: Vec<String>,
    pub(super) append_only: bool,
    pub(super) volatile_cooldown: usize,
    pub(super) stable_prefix_length: usize,
    pub(super) candidate_prefix_length: usize,
    pub(super) candidate_prefix_age: usize,
    pub(super) rewrite_floor: usize,
}

#[derive(Clone, Debug, Default)]
pub(super) struct LiveCommitState {
    pub(super) safe_length: usize,
    pub(super) append_only: bool,
    pub(super) volatile_cooldown: usize,
    pub(super) stable_prefix_length: usize,
    pub(super) candidate_prefix_length: usize,
    pub(super) candidate_prefix_age: usize,
    pub(super) rewrite_floor: usize,
}

pub(super) const VOLATILE_REARM_FRAMES: usize = 30;
pub(super) const STABLE_PREFIX_COMMIT_FRAMES: usize = 30;
pub(super) const TAIL_VOLATILITY_ROWS: usize = 4;

#[derive(Default)]
pub struct Transcript {
    pub(super) committed_rows: usize,
    pub(super) snapshots: HashMap<String, LiveDiffSnapshot>,
    /// When true (mux/embedded mode), only commit_safe rows may enter scrollback.
    /// Snapshot-only rows are excluded because the embedded vt100 scrollback is
    /// irreversible — committing them before finalization causes duplicate rows.
    /// In standalone mode (false) the real terminal scrollback is separate from
    /// the live area, so snapshot-safe rows can be committed without duplicates.
    pub(super) embedded: bool,
}
