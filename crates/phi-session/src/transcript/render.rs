use super::types::*;

pub(super) fn is_plain_blank(line: &str) -> bool {
    line.chars().all(char::is_whitespace)
}

pub(super) fn strip_plain_blank_edges(lines: &[String]) -> Vec<String> {
    let mut start = 0usize;
    let mut end = lines.len();
    while start < end && is_plain_blank(&lines[start]) {
        start += 1;
    }
    while end > start && is_plain_blank(&lines[end - 1]) {
        end -= 1;
    }
    lines[start..end].to_vec()
}

impl Transcript {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        self.committed_rows = 0;
    }

    pub fn committed_rows(&self) -> usize {
        self.committed_rows
    }

    pub fn render<'a>(
        &mut self,
        viewport_height: usize,
        blocks: &[BlockSpec<'a>],
    ) -> Frame {
        let viewport_height = viewport_height.max(1);

        let mut full = Vec::new();

        let count = blocks.len();
        let mut live_start_index = count;
        for (i, block) in blocks.iter().enumerate() {
            if !block.finalized {
                live_start_index = i;
                break;
            }
        }

        let mut live_region_start = None;
        let mut live_recorded = false;
        let mut row = 0usize;

        for (i, block) in blocks.iter().enumerate() {
            let contribution = strip_plain_blank_edges(block.lines);
            if contribution.is_empty() {
                continue;
            }

            let sep = usize::from(
                row > 0 && !is_plain_blank(full.last().map(String::as_str).unwrap_or("")),
            );
            if sep == 1 {
                full.push(String::new());
            }

            if !live_recorded && i >= live_start_index {
                live_region_start = Some(row + sep);
                live_recorded = true;
            }

            full.extend(contribution.iter().cloned());
            row = full.len();
        }

        if self.committed_rows > full.len() {
            self.committed_rows = 0;
        }

        let durable_end = live_region_start.unwrap_or(full.len());
        let window_top = std::cmp::max(
            self.committed_rows,
            full.len().saturating_sub(viewport_height),
        );
        let next_committed =
            std::cmp::max(self.committed_rows, std::cmp::min(durable_end, window_top));
        let scrollback = if next_committed > self.committed_rows {
            full[self.committed_rows..next_committed].to_vec()
        } else {
            Vec::new()
        };
        self.committed_rows = next_committed;
        let live = full[self.committed_rows..].to_vec();

        Frame {
            scrollback,
            live,
            full,
            live_region_start,
            committed_rows: self.committed_rows,
        }
    }
}
