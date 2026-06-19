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

pub(super) fn normalize_row(line: &str) -> String {
    phi_tui::strip_ansi(line).trim_end().to_owned()
}

pub(super) fn rows_visibly_equal(prev: &str, cur: &str) -> bool {
    prev == cur || normalize_row(prev) == normalize_row(cur)
}

pub(super) fn row_visibly_grew(prev: &str, cur: &str) -> bool {
    normalize_row(cur).starts_with(&normalize_row(prev))
}

pub(super) fn common_prefix_length(prev: &[String], cur: &[String]) -> usize {
    let limit = prev.len().min(cur.len());
    let mut i = 0usize;
    while i < limit && rows_visibly_equal(&prev[i], &cur[i]) {
        i += 1;
    }
    i
}

pub(super) fn common_suffix_length(prev: &[String], cur: &[String], prefix_length: usize) -> usize {
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

pub(super) fn derive_live_commit_state(
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
