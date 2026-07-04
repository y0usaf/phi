use super::types::*;

pub(super) fn is_plain_blank(line: &str) -> bool {
    line.chars().all(char::is_whitespace)
}

/// The interior of `lines` with all-whitespace edge lines trimmed off.
pub(super) fn strip_plain_blank_edges(lines: &[String]) -> &[String] {
    let Some(start) = lines.iter().position(|line| !is_plain_blank(line)) else {
        return &[];
    };
    let end = lines
        .iter()
        .rposition(|line| !is_plain_blank(line))
        .map_or(start, |i| i + 1);
    &lines[start..end]
}

impl Transcript {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget commit progress AND the cached durable assembly: the next
    /// render re-emits every finalized row as scrollback. Used on terminal
    /// resize, where the viewport is cleared and history must be replayed
    /// at the new width.
    pub fn reset(&mut self) {
        self.committed_rows = 0;
        self.durable.clear();
        self.durable_blocks.clear();
    }

    pub fn committed_rows(&self) -> usize {
        self.committed_rows
    }

    /// The cached durable rows (assembled finalized prefix). Exposed for
    /// tests asserting on the full assembled transcript.
    pub fn durable_rows(&self) -> &[String] {
        &self.durable
    }

    /// Render the block list into a frame.
    ///
    /// Finalized blocks form the *durable prefix* — everything before the
    /// first non-finalized block. Their assembled rows are cached across
    /// frames, so a steady-state render costs O(live region), not O(entire
    /// history).
    ///
    /// `generation` is the caller's content epoch: within one generation a
    /// finalized block's lines are immutable for its key and the prefix is
    /// append-only. Callers must bump it whenever previously supplied
    /// content may have changed (re-render at a new width, expand toggles,
    /// consolidated cards). A bump rebuilds the assembly while preserving
    /// commit progress. As a safety net, a prefix that diverges from the
    /// cache (key or line-count mismatch, shrunk block list) also triggers
    /// a rebuild, so a missed bump self-heals on any structural change.
    pub fn render<'a>(
        &mut self,
        _viewport_height: usize,
        generation: u64,
        blocks: &[BlockSpec<'a>],
    ) -> Frame {
        let prefix_len = blocks
            .iter()
            .position(|block| !block.finalized)
            .unwrap_or(blocks.len());

        let cache_valid = self.generation == generation
            && self.durable_blocks.len() <= prefix_len
            && self
                .durable_blocks
                .iter()
                .zip(blocks)
                .all(|((key, len), block)| block.key == key && block.lines.len() == *len);
        if !cache_valid {
            self.durable.clear();
            self.durable_blocks.clear();
            self.generation = generation;
        }

        // Append newly finalized blocks to the durable assembly. Stripped
        // contributions never end blank, so exactly one blank separator goes
        // between visible blocks.
        for block in &blocks[self.durable_blocks.len()..prefix_len] {
            let contribution = strip_plain_blank_edges(block.lines);
            self.durable_blocks
                .push((block.key.to_owned(), block.lines.len()));
            if contribution.is_empty() {
                continue;
            }
            if !self.durable.is_empty() {
                self.durable.push(String::new());
            }
            self.durable.extend_from_slice(contribution);
        }

        // Assemble the live region fresh each frame: every block from the
        // first non-finalized one on (finalized blocks after a pending one
        // included — they re-enter the durable prefix once it resolves).
        // `live_sep` is the blank row between the durable and live regions;
        // it belongs to the committed side (it is at live_region_start - 1).
        let mut live_rows: Vec<String> = Vec::new();
        let mut live_sep = 0usize;
        for block in &blocks[prefix_len..] {
            let contribution = strip_plain_blank_edges(block.lines);
            if contribution.is_empty() {
                continue;
            }
            if live_rows.is_empty() {
                live_sep = usize::from(!self.durable.is_empty());
            } else {
                live_rows.push(String::new());
            }
            live_rows.extend_from_slice(contribution);
        }

        let total_rows = self.durable.len() + live_sep + live_rows.len();
        if self.committed_rows > total_rows {
            self.committed_rows = 0;
        }

        let live_region_start = (!live_rows.is_empty()).then(|| self.durable.len() + live_sep);
        let durable_end = live_region_start.unwrap_or(self.durable.len());
        let next_committed = std::cmp::max(self.committed_rows, durable_end);

        // Newly committed rows: durable rows in range, plus the durable/live
        // separator when the range covers it (row index self.durable.len()).
        let mut scrollback = Vec::new();
        if next_committed > self.committed_rows {
            let durable_slice_end = next_committed.min(self.durable.len());
            if self.committed_rows < durable_slice_end {
                scrollback.extend_from_slice(&self.durable[self.committed_rows..durable_slice_end]);
            }
            if next_committed > self.durable.len() && self.committed_rows <= self.durable.len() {
                scrollback.push(String::new());
            }
        }
        self.committed_rows = next_committed;

        // Live rows after the committed boundary. `next_committed` is always
        // >= durable.len() (durable_end is), so the skip only ever trims the
        // separator and rows committed by an earlier, longer frame.
        let skip = next_committed - self.durable.len().min(next_committed);
        let live = if skip <= live_sep {
            live_rows
        } else {
            live_rows.split_off(skip - live_sep)
        };

        Frame {
            scrollback,
            live,
            live_region_start,
            committed_rows: self.committed_rows,
        }
    }
}
