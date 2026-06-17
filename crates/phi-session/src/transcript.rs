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
struct LiveDiffSnapshot {
    width: usize,
    lines: Vec<String>,
    append_only: bool,
    volatile_cooldown: usize,
    stable_prefix_length: usize,
    candidate_prefix_length: usize,
    candidate_prefix_age: usize,
    rewrite_floor: usize,
}

#[derive(Clone, Debug, Default)]
struct LiveCommitState {
    safe_length: usize,
    append_only: bool,
    volatile_cooldown: usize,
    stable_prefix_length: usize,
    candidate_prefix_length: usize,
    candidate_prefix_age: usize,
    rewrite_floor: usize,
}

const VOLATILE_REARM_FRAMES: usize = 30;
const STABLE_PREFIX_COMMIT_FRAMES: usize = 30;
const TAIL_VOLATILITY_ROWS: usize = 4;

#[derive(Default)]
pub struct Transcript {
    committed_rows: usize,
    snapshots: HashMap<String, LiveDiffSnapshot>,
    /// When true (mux/embedded mode), only commit_safe rows may enter scrollback.
    /// Snapshot-only rows are excluded because the embedded vt100 scrollback is
    /// irreversible — committing them before finalization causes duplicate rows.
    /// In standalone mode (false) the real terminal scrollback is separate from
    /// the live area, so snapshot-safe rows can be committed without duplicates.
    embedded: bool,
}

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

fn is_plain_blank(line: &str) -> bool {
    line.chars().all(char::is_whitespace)
}

fn strip_plain_blank_edges(lines: &[String]) -> Vec<String> {
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

fn normalize_row(line: &str) -> String {
    phi_tui::strip_ansi(line).trim_end().to_owned()
}

fn rows_visibly_equal(prev: &str, cur: &str) -> bool {
    prev == cur || normalize_row(prev) == normalize_row(cur)
}

fn row_visibly_grew(prev: &str, cur: &str) -> bool {
    normalize_row(cur).starts_with(&normalize_row(prev))
}

fn common_prefix_length(prev: &[String], cur: &[String]) -> usize {
    let limit = prev.len().min(cur.len());
    let mut i = 0usize;
    while i < limit && rows_visibly_equal(&prev[i], &cur[i]) {
        i += 1;
    }
    i
}

fn common_suffix_length(prev: &[String], cur: &[String], prefix_length: usize) -> usize {
    let limit = (prev.len() - prefix_length).min(cur.len() - prefix_length);
    let mut i = 0usize;
    while i < limit
        && rows_visibly_equal(
            &prev[prev.len().saturating_sub(1 + i)],
            &cur[cur.len().saturating_sub(1 + i)],
        )
    {
        i += 1;
    }
    i
}

fn derive_live_commit_state(
    previous: Option<&LiveDiffSnapshot>,
    width: usize,
    current: &[String],
) -> LiveCommitState {
    let Some(previous) = previous.filter(|snapshot| snapshot.width == width) else {
        return LiveCommitState::default();
    };

    let mut append_only = previous.append_only;
    let mut volatile_cooldown = previous.volatile_cooldown;
    let mut stable_prefix_length = previous.stable_prefix_length;
    let mut candidate_prefix_length = previous.candidate_prefix_length;
    let mut candidate_prefix_age = previous.candidate_prefix_age;
    let mut rewrite_floor = previous.rewrite_floor;
    let mut trailing_row_growth = false;

    let prefix_length = common_prefix_length(&previous.lines, current);
    let static_render = prefix_length == previous.lines.len() && prefix_length == current.len();
    let mut clean_frame = true;

    if !static_render {
        let suffix_length = common_suffix_length(&previous.lines, current, prefix_length);
        let preserved_every_row = prefix_length + suffix_length >= previous.lines.len();
        let tail_confined = preserved_every_row
            || prefix_length >= previous.lines.len().saturating_sub(TAIL_VOLATILITY_ROWS);
        if tail_confined && current.len() >= previous.lines.len() {
            trailing_row_growth = current.len() == previous.lines.len()
                && prefix_length == previous.lines.len().saturating_sub(1)
                && previous
                    .lines
                    .get(prefix_length)
                    .zip(current.get(prefix_length))
                    .is_some_and(|(prev, cur)| row_visibly_grew(prev, cur));
            if volatile_cooldown == 0 {
                append_only = true;
            }
            let delta = current.len() as isize - previous.lines.len() as isize;
            if delta > 0 && rewrite_floor != usize::MAX {
                let suffix_start = std::cmp::max(
                    prefix_length,
                    previous.lines.len().saturating_sub(suffix_length),
                );
                if rewrite_floor >= suffix_start {
                    rewrite_floor = ((rewrite_floor as isize) + delta) as usize;
                }
            }
        } else {
            clean_frame = false;
            append_only = false;
            volatile_cooldown = VOLATILE_REARM_FRAMES;
        }
    }

    if clean_frame && volatile_cooldown > 0 {
        volatile_cooldown -= 1;
    }

    if prefix_length < stable_prefix_length {
        rewrite_floor = rewrite_floor.min(prefix_length);
        stable_prefix_length = prefix_length;
        candidate_prefix_length = prefix_length;
        candidate_prefix_age = 0;
    } else {
        candidate_prefix_length = if candidate_prefix_age == 0 {
            prefix_length
        } else {
            candidate_prefix_length.min(prefix_length)
        };
        candidate_prefix_age += 1;
        if candidate_prefix_age >= STABLE_PREFIX_COMMIT_FRAMES {
            stable_prefix_length = candidate_prefix_length
                .min(rewrite_floor)
                .min(current.len().saturating_sub(TAIL_VOLATILITY_ROWS));
            candidate_prefix_length = prefix_length;
            candidate_prefix_age = 0;
        }
    }

    let safe_length = if append_only {
        if trailing_row_growth {
            current.len()
        } else {
            stable_prefix_length.max(current.len().saturating_sub(TAIL_VOLATILITY_ROWS))
        }
    } else {
        stable_prefix_length
    };

    LiveCommitState {
        safe_length,
        append_only,
        volatile_cooldown,
        stable_prefix_length,
        candidate_prefix_length,
        candidate_prefix_age,
        rewrite_floor,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn render(
        tx: &mut Transcript,
        width: usize,
        viewport_height: usize,
        inputs: &[(&str, Vec<String>, bool, bool)],
    ) -> Frame {
        let specs: Vec<BlockSpec<'_>> = inputs
            .iter()
            .map(|(key, lines, finalized, commit_stable)| BlockSpec {
                key,
                lines,
                finalized: *finalized,
                commit_stable: *commit_stable,
                version: 0,
            })
            .collect();
        tx.render(width, viewport_height, &specs)
    }

    #[test]
    fn exactly_one_blank_separator_between_visible_blocks() {
        let mut tx = Transcript::new();
        let frame = render(
            &mut tx,
            80,
            20,
            &[
                ("a", vec!["".into(), "a".into(), "".into()], true, true),
                ("b", vec!["b".into()], true, true),
            ],
        );
        assert_eq!(frame.full, vec!["a", "", "b"]);
    }

    #[test]
    fn earliest_unfinalized_block_starts_live_region() {
        let mut tx = Transcript::new();
        let frame = render(
            &mut tx,
            80,
            20,
            &[
                ("done", vec!["done".into()], true, true),
                ("pending", vec!["pending".into()], false, true),
                ("card", vec!["card".into()], true, true),
            ],
        );
        assert_eq!(frame.full, vec!["done", "", "pending", "", "card"]);
        assert_eq!(frame.live_region_start, Some(2));
    }

    #[test]
    fn provisional_live_block_never_offers_snapshot_beyond_safe_prefix() {
        let mut tx = Transcript::new();
        for _ in 0..40 {
            let frame = render(
                &mut tx,
                80,
                20,
                &[
                    ("history", vec!["history".into()], true, true),
                    (
                        "preview",
                        vec![
                            "┌ Edit: foo.ts".into(),
                            "… (2 more hunks above)".into(),
                            "-old-a".into(),
                            "+new-a".into(),
                            "└ (streaming)".into(),
                        ],
                        false,
                        false,
                    ),
                ],
            );
            assert_eq!(frame.live_region_start, Some(2));
            assert_eq!(frame.commit_safe_end, None);
            assert_eq!(frame.snapshot_safe_end, None);
        }
    }

    #[test]
    fn trailing_line_growth_is_offerable() {
        let mut tx = Transcript::new();
        let first = render(
            &mut tx,
            80,
            20,
            &[(
                "stream",
                vec![
                    "para one".into(),
                    "para two".into(),
                    "the quick brown".into(),
                ],
                false,
                true,
            )],
        );
        assert_eq!(first.commit_safe_end, None);

        let second = render(
            &mut tx,
            80,
            20,
            &[(
                "stream",
                vec![
                    "para one".into(),
                    "para two".into(),
                    "the quick brown fox".into(),
                ],
                false,
                true,
            )],
        );
        assert_eq!(second.commit_safe_end, Some(3));
    }

    #[test]
    fn snapshot_safe_live_rows_do_not_enter_terminal_scrollback() {
        let mut tx = Transcript::new_embedded();
        let frame = render(
            &mut tx,
            80,
            3,
            &[(
                "stream",
                vec![
                    "one".into(),
                    "two".into(),
                    "three".into(),
                    "four".into(),
                    "five".into(),
                ],
                false,
                true,
            )],
        );

        assert_eq!(frame.commit_safe_end, None);
        assert_eq!(frame.snapshot_safe_end, Some(5));
        assert!(frame.scrollback.is_empty());
        assert_eq!(frame.committed_rows, 0);
    }

    #[test]
    fn finalized_history_before_unsafe_live_rows_can_still_scroll() {
        let mut tx = Transcript::new_embedded();
        let frame = render(
            &mut tx,
            80,
            3,
            &[
                ("history", vec!["history".into()], true, true),
                (
                    "stream",
                    vec![
                        "one".into(),
                        "two".into(),
                        "three".into(),
                        "four".into(),
                        "five".into(),
                    ],
                    false,
                    true,
                ),
            ],
        );

        assert_eq!(frame.commit_safe_end, None);
        assert_eq!(frame.snapshot_safe_end, Some(7));
        assert_eq!(frame.scrollback, vec!["history", ""]);
        assert_eq!(frame.committed_rows, 2);
    }

    #[test]
    fn viewport_commits_scrolled_safe_rows_only() {
        let mut tx = Transcript::new();
        let frame = render(
            &mut tx,
            80,
            3,
            &[
                ("a", vec!["a".into()], true, true),
                ("b", vec!["b".into()], true, true),
                ("c", vec!["c".into()], true, true),
                ("d", vec!["d".into()], true, true),
            ],
        );
        assert_eq!(frame.full, vec!["a", "", "b", "", "c", "", "d"]);
        assert_eq!(frame.scrollback, vec!["a", "", "b", ""]);
        assert_eq!(frame.live, vec!["c", "", "d"]);
        assert_eq!(frame.committed_rows, 4);
    }
}
