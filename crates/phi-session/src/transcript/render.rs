use std::collections::HashMap;

use super::diff::*;
use super::types::*;

impl Transcript {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn new_embedded() -> Self {
        Self {
            embedded: true,
            ..Self::default()
        }
    }

    pub fn reset(&mut self) {
        self.committed_rows = 0;
        self.snapshots.clear();
    }

    pub fn committed_rows(&self) -> usize {
        self.committed_rows
    }

    pub fn render<'a>(
        &mut self,
        width: usize,
        viewport_height: usize,
        blocks: &[BlockSpec<'a>],
    ) -> Frame {
        let width = width.max(1);
        let viewport_height = viewport_height.max(1);

        let mut full = Vec::new();
        let mut live_region_start = None;
        let mut commit_safe_end = None;
        let mut snapshot_safe_end = None;

        let count = blocks.len();
        let mut live_start_index = count.saturating_sub(1);
        for (i, block) in blocks.iter().enumerate() {
            if !block.finalized {
                live_start_index = i;
                break;
            }
        }

        let mut next_snapshots: HashMap<String, LiveDiffSnapshot> =
            HashMap::with_capacity(blocks.len());
        let mut commit_safe_open = true;
        let mut live_recorded = false;
        let mut row = 0usize;

        for (i, block) in blocks.iter().enumerate() {
            let contribution = strip_plain_blank_edges(block.lines);
            let previous = self.snapshots.get(block.key);
            let live_commit_state =
                if i >= live_start_index && !block.finalized && block.commit_stable {
                    derive_live_commit_state(previous, width, &contribution)
                } else {
                    LiveCommitState::default()
                };

            next_snapshots.insert(
                block.key.to_owned(),
                LiveDiffSnapshot {
                    width,
                    lines: contribution.clone(),
                    append_only: live_commit_state.append_only,
                    volatile_cooldown: live_commit_state.volatile_cooldown,
                    stable_prefix_length: live_commit_state.stable_prefix_length,
                    candidate_prefix_length: live_commit_state.candidate_prefix_length,
                    candidate_prefix_age: live_commit_state.candidate_prefix_age,
                    rewrite_floor: live_commit_state.rewrite_floor,
                },
            );

            if contribution.is_empty() {
                if i >= live_start_index && commit_safe_open && !block.finalized {
                    commit_safe_open = false;
                }
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

            let block_start = row + sep;
            full.extend(contribution.iter().cloned());
            row = full.len();

            if i >= live_start_index && commit_safe_open {
                let safe_length = if block.finalized {
                    contribution.len()
                } else {
                    live_commit_state.safe_length
                };
                if safe_length > 0 {
                    commit_safe_end = Some(block_start + safe_length);
                }
                let snapshot_length = if block.finalized || block.commit_stable {
                    contribution.len()
                } else {
                    safe_length
                };
                if snapshot_length > 0 {
                    snapshot_safe_end = Some(block_start + snapshot_length);
                }
                if !(block.finalized && safe_length >= contribution.len()) {
                    commit_safe_open = false;
                }
            }
        }

        self.snapshots = next_snapshots;

        if full.is_empty() {
            self.committed_rows = 0;
            return Frame {
                scrollback: Vec::new(),
                live: Vec::new(),
                full,
                live_region_start,
                commit_safe_end,
                snapshot_safe_end,
                committed_rows: 0,
            };
        }

        if self.committed_rows > full.len() {
            self.committed_rows = 0;
        }

        // In embedded/mux mode only commit_safe rows may enter scrollback: the
        // embedded vt100 buffer is irreversible, so committing snapshot-only rows
        // before they finalize causes them to appear twice once the block commits.
        // In standalone mode the real terminal scrollback is separate from the live
        // area, so snapshot-safe rows can be committed without producing duplicates;
        // this is essential for long REPL blocks whose content exceeds the viewport.
        let durable_end = if self.embedded {
            commit_safe_end.unwrap_or(live_region_start.unwrap_or(full.len()))
        } else {
            snapshot_safe_end.unwrap_or(commit_safe_end.unwrap_or(full.len()))
        };
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
            commit_safe_end,
            snapshot_safe_end,
            committed_rows: self.committed_rows,
        }
    }
}
