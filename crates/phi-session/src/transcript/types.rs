#[derive(Clone, Debug)]
pub struct BlockSpec<'a> {
    pub key: &'a str,
    pub lines: &'a [String],
    pub finalized: bool,
    /// When set, only the last `max_height` lines are rendered; earlier
    /// lines are replaced by a single "… (N earlier lines)" indicator.
    pub max_height: Option<usize>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Frame {
    pub scrollback: Vec<String>,
    pub live: Vec<String>,
    pub full: Vec<String>,
    pub live_region_start: Option<usize>,
    pub committed_rows: usize,
}

#[derive(Default)]
pub struct Transcript {
    pub(super) committed_rows: usize,
}
