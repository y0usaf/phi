#[derive(Clone, Debug)]
pub struct BlockSpec<'a> {
    pub key: &'a str,
    pub lines: &'a [String],
    pub finalized: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Frame {
    pub scrollback: Vec<String>,
    pub live: Vec<String>,
    pub live_region_start: Option<usize>,
    pub committed_rows: usize,
}

#[derive(Default)]
pub struct Transcript {
    pub(super) committed_rows: usize,
    /// Cached assembled rows of the durable prefix (finalized blocks before
    /// the first non-finalized block), reused across frames so a render
    /// costs O(live region) instead of re-cloning the whole history.
    pub(super) durable: Vec<String>,
    /// Fingerprints (key, source line count) of the blocks assembled into
    /// `durable`, in order. A prefix that stops matching invalidates the
    /// cache.
    pub(super) durable_blocks: Vec<(String, usize)>,
    /// The caller's content generation `durable` was assembled under.
    pub(super) generation: u64,
}
