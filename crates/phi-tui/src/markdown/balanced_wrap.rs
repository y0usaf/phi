use unicode_width::UnicodeWidthStr;

/// Knuth-Plass-style balanced text wrapping using dynamic programming on
/// squared slack. Minimizes the sum of `(width - line_width)²` across all
/// lines (including the last) so that leftover space is distributed evenly —
/// ideal for centered or right-aligned text where visual balance matters.
///
/// Words are joined by single spaces. A single word wider than `width` is
/// placed on its own line (no character-level splitting).
pub fn wrap_line_balanced(words: &[&str], width: usize) -> Vec<String> {
    if words.is_empty() {
        return vec![];
    }

    let n = words.len();
    let word_widths: Vec<usize> = words
        .iter()
        .map(|w| UnicodeWidthStr::width(*w))
        .collect();

    // dp[i] = minimum total cost to wrap words[i..n]
    // breaks[i] = exclusive end index of the first line starting at word i
    // line_counts[i] = number of lines used in the optimal solution from i
    let mut dp = vec![usize::MAX; n + 1];
    let mut breaks = vec![0usize; n];
    let mut line_counts = vec![usize::MAX; n + 1];
    dp[n] = 0;
    line_counts[n] = 0;

    for start in (0..n).rev() {
        let mut line_width = 0usize;
        for end in start..n {
            if end == start {
                line_width = word_widths[end];
            } else {
                // single space separator between words
                line_width = line_width
                    .saturating_add(1)
                    .saturating_add(word_widths[end]);
            }

            // Stop extending this line once it exceeds width (unless it's a
            // single oversized word that must go on its own line).
            if line_width > width && end > start {
                break;
            }

            if dp[end + 1] == usize::MAX {
                continue;
            }

            let slack = width.saturating_sub(line_width);
            let cost = slack
                .saturating_mul(slack)
                .saturating_add(dp[end + 1]);
            let lines_used = 1usize.saturating_add(line_counts[end + 1]);

            let should_replace = cost < dp[start]
                || (cost == dp[start] && lines_used < line_counts[start])
                || (cost == dp[start]
                    && lines_used == line_counts[start]
                    && line_width < line_width_for_break(&word_widths, start, breaks[start]));

            if should_replace {
                dp[start] = cost;
                breaks[start] = end + 1;
                line_counts[start] = lines_used;
            }
        }
    }

    // If no valid solution was found (shouldn't happen), fall back to one word
    // per line.
    if dp[0] == usize::MAX {
        return words.iter().map(|w| w.to_string()).collect();
    }

    // Reconstruct lines from the break points.
    let mut result = Vec::new();
    let mut start = 0usize;
    while start < n {
        let end = breaks[start].min(n);
        if end <= start {
            // Defensive: should not happen after dp[0] != MAX check.
            result.push(words[start].to_string());
            start += 1;
            continue;
        }
        let line = words[start..end].join(" ");
        result.push(line);
        start = end;
    }

    result
}

/// Compute the rendered width of words[start..end) joined by single spaces.
fn line_width_for_break(word_widths: &[usize], start: usize, end: usize) -> usize {
    if end <= start {
        return usize::MAX;
    }
    let mut width = 0usize;
    for idx in start..end {
        if idx > start {
            width = width.saturating_add(1);
        }
        width = width.saturating_add(word_widths[idx]);
    }
    width
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input() {
        assert_eq!(wrap_line_balanced(&[], 80), Vec::<String>::new());
    }

    #[test]
    fn single_word() {
        assert_eq!(
            wrap_line_balanced(&["hello"], 80),
            vec!["hello".to_string()]
        );
    }

    #[test]
    fn fits_on_one_line() {
        assert_eq!(
            wrap_line_balanced(&["hello", "world"], 80),
            vec!["hello world".to_string()]
        );
    }

    #[test]
    fn must_wrap() {
        // width 11 can hold "hello world" (11 chars) but barely; width 10
        // forces a split.
        let result = wrap_line_balanced(&["hello", "world"], 10);
        assert_eq!(result, vec!["hello".to_string(), "world".to_string()]);
    }

    #[test]
    fn balanced_split() {
        // 5 words, width fits all on one line with small slack → one line.
        let words = vec!["one", "two", "three", "four", "five"];
        let result = wrap_line_balanced(&words, 80);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], "one two three four five");
    }

    #[test]
    fn oversized_word() {
        // A single word wider than the width goes on its own line.
        let result = wrap_line_balanced(&["abcdefghijklmnopqrstuvwxyz"], 5);
        assert_eq!(result, vec!["abcdefghijklmnopqrstuvwxyz".to_string()]);
    }

    #[test]
    fn three_way_split() {
        // With width 8, "aaa bbb ccc" (11 chars) must split.
        // "aaa bbb" (7) + "ccc" (3) → cost 1+25=26
        // "aaa" (3) + "bbb ccc" (7) → cost 25+1=26
        // Tie-breaker prefers narrower first line → "aaa" then "bbb ccc".
        let result = wrap_line_balanced(&["aaa", "bbb", "ccc"], 8);
        assert_eq!(result.len(), 2);
        assert!(result[0] == "aaa bbb" || result[0] == "aaa");
    }

    #[test]
    fn unicode_width() {
        // CJK characters are double-width: "你好" = 4, "世界" = 4, joined = 9.
        // With width 9 it fits on one line; width 8 forces a split.
        let result = wrap_line_balanced(&["你好", "世界"], 9);
        assert_eq!(result, vec!["你好 世界".to_string()]);

        let result = wrap_line_balanced(&["你好", "世界"], 8);
        assert_eq!(result, vec!["你好".to_string(), "世界".to_string()]);
    }
}
