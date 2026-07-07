//! Shared tool-output truncation (pi parity): two independent limits,
//! whichever hits first — lines and bytes. `head` keeps the beginning
//! (read), `tail` keeps the end (bash).

pub const MAX_LINES: usize = 2000;
pub const MAX_BYTES: usize = 50 * 1024;

#[derive(Debug)]
pub struct Truncation {
    pub text: String,
    pub truncated: bool,
    /// Lines kept in `text`.
    pub kept: usize,
    /// Total lines in the input.
    pub total: usize,
}

/// Keep the first lines that fit. Never returns a partial line unless the
/// very first line alone exceeds the byte budget.
pub fn head(input: &str, max_lines: usize, max_bytes: usize) -> Truncation {
    let total = input.lines().count();
    let mut out = String::new();
    let mut kept = 0;
    for line in input.lines() {
        if kept >= max_lines || out.len() + line.len() + 1 > max_bytes {
            break;
        }
        out.push_str(line);
        out.push('\n');
        kept += 1;
    }
    if kept == 0 && total > 0 {
        // First line alone blows the budget: keep a byte-safe prefix.
        let first = input.lines().next().unwrap_or("");
        let cut = floor_char_boundary(first, max_bytes.min(first.len()));
        return Truncation {
            text: first[..cut].to_owned(),
            truncated: true,
            kept: 1,
            total,
        };
    }
    Truncation {
        text: out,
        truncated: kept < total,
        kept,
        total,
    }
}

/// Keep the last lines that fit.
pub fn tail(input: &str, max_lines: usize, max_bytes: usize) -> Truncation {
    let lines: Vec<&str> = input.lines().collect();
    let total = lines.len();
    let mut bytes = 0;
    let mut start = total;
    while start > 0 {
        let cost = lines[start - 1].len() + 1;
        if total - start >= max_lines || bytes + cost > max_bytes {
            break;
        }
        bytes += cost;
        start -= 1;
    }
    if start == total && total > 0 {
        // Last line alone blows the budget: keep its byte-safe suffix.
        let last = lines[total - 1];
        let cut = ceil_char_boundary(last, last.len().saturating_sub(max_bytes));
        return Truncation {
            text: last[cut..].to_owned(),
            truncated: true,
            kept: 1,
            total,
        };
    }
    let mut text = lines[start..].join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    Truncation {
        text,
        truncated: start > 0,
        kept: total - start,
        total,
    }
}

fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_char_boundary(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_keeps_everything_under_the_limits() {
        let t = head("a\nb\nc\n", 10, 1024);
        assert_eq!(t.text, "a\nb\nc\n");
        assert!(!t.truncated);
        assert_eq!((t.kept, t.total), (3, 3));
    }

    #[test]
    fn head_cuts_at_line_limit() {
        let t = head("a\nb\nc\nd\n", 2, 1024);
        assert_eq!(t.text, "a\nb\n");
        assert!(t.truncated);
        assert_eq!((t.kept, t.total), (2, 4));
    }

    #[test]
    fn head_cuts_at_byte_limit_on_line_boundary() {
        let t = head("aaaa\nbbbb\ncccc\n", 10, 11);
        assert_eq!(t.text, "aaaa\nbbbb\n");
        assert!(t.truncated);
        assert_eq!(t.kept, 2);
    }

    #[test]
    fn head_giant_first_line_keeps_prefix() {
        let long = "x".repeat(100);
        let t = head(&long, 10, 10);
        assert_eq!(t.text.len(), 10);
        assert!(t.truncated);
    }

    #[test]
    fn tail_keeps_the_end() {
        let t = tail("a\nb\nc\nd\n", 2, 1024);
        assert_eq!(t.text, "c\nd\n");
        assert!(t.truncated);
        assert_eq!((t.kept, t.total), (2, 4));
    }

    #[test]
    fn tail_untruncated_round_trips() {
        let t = tail("a\nb\n", 10, 1024);
        assert_eq!(t.text, "a\nb\n");
        assert!(!t.truncated);
    }

    #[test]
    fn boundaries_respect_utf8() {
        let s = "é".repeat(50);
        let h = head(&s, 1, 7);
        assert!(s.starts_with(&h.text));
        let t = tail(&s, 1, 7);
        assert!(s.ends_with(&t.text));
    }
}
