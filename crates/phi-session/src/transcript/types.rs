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
    pub full: Vec<String>,
    pub live_region_start: Option<usize>,
    pub committed_rows: usize,
}

#[derive(Default)]
pub struct Transcript {
    pub(super) committed_rows: usize,
}
