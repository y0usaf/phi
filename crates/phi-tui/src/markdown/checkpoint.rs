//! Block-boundary checkpointing for partial markdown re-renders.
//!
//! Finds safe byte offsets for partial re-renders by detecting closed code
//! fences, closed display-math blocks, completed headings, and blank-line
//! separators. An incremental renderer can use the returned offset to avoid
//! re-parsing from byte 0 when only trailing content has changed.

/// Kind of markdown block, used to determine spacing behaviour at checkpoint
/// boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkdownBlockKind {
    Heading,
    Paragraph,
    List,
    BlockQuote,
    CodeBlock,
    DisplayMath,
    Rule,
    HtmlBlock,
}

/// A checkpoint marking the byte offset immediately after a complete block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompleteBlockCheckpoint {
    /// Byte offset (exclusive) of the checkpoint — text up to and including
    /// this position forms complete blocks that are safe to cache.
    pub offset: usize,
    /// Whether a blank separator line should be preserved at this boundary
    /// when splicing incremental renders.
    pub needs_separator: bool,
}

impl CompleteBlockCheckpoint {
    /// Convenience: return just the byte offset, or 0 if no checkpoint.
    pub fn offset_or_zero(&self) -> usize {
        self.offset
    }
}

/// Find the last complete-block checkpoint in `text`.
///
/// Scans line-by-line tracking code-fence state and display-math (`$$`)
/// state. Returns `Some(checkpoint)` positioned just past the last line that
/// completes a block (closed fence, closed math, heading, or blank-line
/// separator). Returns `None` when no complete block boundary is found.
pub fn find_last_complete_block_checkpoint(text: &str) -> Option<CompleteBlockCheckpoint> {
    if text.is_empty() {
        return None;
    }
    let mut checkpoint = None;
    let mut line_start = 0usize;
    let mut fence_state: Option<(char, usize)> = None;
    let mut display_math_open = false;
    let mut last_nonblank_kind: Option<MarkdownBlockKind> = None;

    while line_start <= text.len() {
        let relative_end = text[line_start..].find('\n');
        let (line_end, line_ends_with_newline) = match relative_end {
            Some(end) => (line_start + end, true),
            None => (text.len(), false),
        };
        let line = &text[line_start..line_end];
        let line_end_including_newline = if line_ends_with_newline {
            line_end + 1
        } else {
            line_end
        };

        match fence_state {
            Some((fence_char, fence_len)) => {
                if is_closing_fence(line, fence_char, fence_len) {
                    fence_state = None;
                    last_nonblank_kind = Some(MarkdownBlockKind::CodeBlock);
                    checkpoint = Some(CompleteBlockCheckpoint {
                        offset: line_end_including_newline,
                        needs_separator: true,
                    });
                }
            }
            None => {
                if display_math_open {
                    let dd_count = count_unescaped_double_dollar(line);
                    if dd_count % 2 == 1 {
                        display_math_open = false;
                        last_nonblank_kind = Some(MarkdownBlockKind::DisplayMath);
                        checkpoint = Some(CompleteBlockCheckpoint {
                            offset: line_end_including_newline,
                            needs_separator: true,
                        });
                    }
                } else if let Some((fence_char, fence_len)) = parse_opening_fence(line) {
                    fence_state = Some((fence_char, fence_len));
                } else {
                    let dd_count = count_unescaped_double_dollar(line);
                    if dd_count > 0 {
                        if dd_count % 2 == 1 {
                            display_math_open = true;
                        } else {
                            last_nonblank_kind = Some(MarkdownBlockKind::DisplayMath);
                            checkpoint = Some(CompleteBlockCheckpoint {
                                offset: line_end_including_newline,
                                needs_separator: true,
                            });
                        }
                    } else if line_ends_with_newline && is_heading_line(line.trim_start()) {
                        last_nonblank_kind = Some(MarkdownBlockKind::Heading);
                        checkpoint = Some(CompleteBlockCheckpoint {
                            offset: line_end_including_newline,
                            needs_separator: true,
                        });
                    } else if line_ends_with_newline && line.trim().is_empty() {
                        checkpoint = Some(CompleteBlockCheckpoint {
                            offset: line_end_including_newline,
                            needs_separator: last_nonblank_kind.is_some(),
                        });
                    } else {
                        last_nonblank_kind = Some(infer_markdown_line_kind(line));
                    }
                }
            }
        }

        if !line_ends_with_newline {
            break;
        }
        line_start = line_end + 1;
    }

    checkpoint
}

/// Convenience wrapper returning just the byte offset of the last complete
/// block checkpoint, or `None` if none was found.
pub fn find_last_complete_block(text: &str) -> Option<usize> {
    find_last_complete_block_checkpoint(text).map(|cp| cp.offset)
}

// ─── Helper functions (ported from jcode) ──────────────────────────────

/// Parse a markdown code-fence opening line (``` or ~~~).
/// Returns `(fence_char, fence_len)` if the line opens a fence.
pub(crate) fn parse_opening_fence(line: &str) -> Option<(char, usize)> {
    let indent = line.chars().take_while(|c| *c == ' ').count();
    if indent > 3 {
        return None;
    }
    let trimmed = &line[indent..];
    let first = trimmed.chars().next()?;
    if first != '`' && first != '~' {
        return None;
    }

    let fence_len = trimmed.chars().take_while(|c| *c == first).count();
    if fence_len < 3 {
        return None;
    }

    Some((first, fence_len))
}

/// Check if `line` is a closing fence matching `fence_char` with at least
/// `min_len` repetitions.
pub(crate) fn is_closing_fence(line: &str, fence_char: char, min_len: usize) -> bool {
    let indent = line.chars().take_while(|c| *c == ' ').count();
    if indent > 3 {
        return false;
    }
    let trimmed = &line[indent..];

    let fence_len = trimmed.chars().take_while(|c| *c == fence_char).count();
    if fence_len < min_len {
        return false;
    }

    trimmed[fence_len..].trim().is_empty()
}

/// Count unescaped `$$` sequences in a line (for display-math detection).
pub(crate) fn count_unescaped_double_dollar(line: &str) -> usize {
    let bytes = line.as_bytes();
    let mut count = 0usize;
    let mut ix = 0usize;

    while ix + 1 < bytes.len() {
        if bytes[ix] == b'\\' {
            ix += 2;
            continue;
        }
        if bytes[ix] == b'$' && bytes[ix + 1] == b'$' {
            count += 1;
            ix += 2;
            continue;
        }
        ix += 1;
    }

    count
}

/// Check if a line is a markdown heading (`#` through `######`).
pub(crate) fn is_heading_line(line: &str) -> bool {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    hashes > 0 && hashes <= 6 && line.chars().nth(hashes) == Some(' ')
}

/// Check if a line is a thematic break (`---`, `***`, `___`).
pub(crate) fn is_thematic_break_line(line: &str) -> bool {
    let trimmed = line.trim();
    let mut marker: Option<char> = None;
    let mut count = 0usize;

    for ch in trimmed.chars() {
        if ch == ' ' || ch == '\t' {
            continue;
        }
        match marker {
            None if matches!(ch, '-' | '*' | '_') => {
                marker = Some(ch);
                count = 1;
            }
            Some(existing) if ch == existing => count += 1,
            _ => return false,
        }
    }

    count >= 3
}

/// Check if a line looks like an ordered list item (`1. `, `2) `, etc.).
pub(crate) fn looks_like_ordered_list_item(line: &str) -> bool {
    let trimmed = line.trim_start();
    let digit_count = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
    digit_count > 0
        && matches!(trimmed.chars().nth(digit_count), Some('.' | ')'))
        && matches!(trimmed.chars().nth(digit_count + 1), Some(' ' | '\t'))
}

/// Infer the markdown block kind from a non-blank, non-fence line.
pub(crate) fn infer_markdown_line_kind(line: &str) -> MarkdownBlockKind {
    let trimmed = line.trim_start();
    if is_heading_line(trimmed) {
        MarkdownBlockKind::Heading
    } else if is_thematic_break_line(trimmed) {
        MarkdownBlockKind::Rule
    } else if trimmed.starts_with('>') {
        MarkdownBlockKind::BlockQuote
    } else if trimmed.starts_with("- ")
        || trimmed.starts_with("* ")
        || trimmed.starts_with("+ ")
        || looks_like_ordered_list_item(trimmed)
    {
        MarkdownBlockKind::List
    } else if trimmed.starts_with('<') {
        MarkdownBlockKind::HtmlBlock
    } else {
        MarkdownBlockKind::Paragraph
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_code_fence_produces_checkpoint() {
        let text = "```rust\nfn main() {}\n```\n";
        let cp = find_last_complete_block_checkpoint(text).unwrap();
        assert_eq!(cp.offset, text.len());
    }

    #[test]
    fn unclosed_code_fence_produces_no_checkpoint_after_open() {
        let text = "```rust\nfn main() {}\n";
        // No closing fence — checkpoint should be at the opening fence line end
        // (the blank-line / heading logic doesn't fire, but the fence opening itself
        // doesn't create a checkpoint). However there may be no checkpoint at all.
        // The text before the fence has no complete block, so None is expected.
        let cp = find_last_complete_block_checkpoint(text);
        // Without any complete block before the unclosed fence, expect None
        assert!(cp.is_none() || cp.unwrap().offset < text.len());
    }

    #[test]
    fn closed_display_math_produces_checkpoint() {
        let text = "$$\nx^2 + y^2\n$$\n";
        let cp = find_last_complete_block_checkpoint(text).unwrap();
        assert_eq!(cp.offset, text.len());
    }

    #[test]
    fn heading_produces_checkpoint() {
        let text = "# Title\nbody text\n";
        let cp = find_last_complete_block_checkpoint(text).unwrap();
        assert_eq!(cp.offset, "# Title\n".len());
    }

    #[test]
    fn blank_line_after_paragraph_produces_checkpoint() {
        let text = "some text\n\nmore";
        let cp = find_last_complete_block_checkpoint(text).unwrap();
        assert_eq!(cp.offset, "some text\n\n".len());
    }

    #[test]
    fn multiple_closed_finds_last() {
        let text = "```\nblock1\n```\n\n```\nblock2\n```\n";
        let cp = find_last_complete_block_checkpoint(text).unwrap();
        assert_eq!(cp.offset, text.len());
    }

    #[test]
    fn find_last_complete_block_returns_offset() {
        let text = "# Hello\n";
        assert_eq!(find_last_complete_block(text), Some(text.len()));
    }

    #[test]
    fn empty_text_returns_none() {
        assert_eq!(find_last_complete_block_checkpoint(""), None);
    }

    #[test]
    fn parse_opening_fence_basic() {
        assert_eq!(parse_opening_fence("```rust"), Some(('`', 3)));
        assert_eq!(parse_opening_fence("~~~"), Some(('~', 3)));
        assert_eq!(parse_opening_fence("``````"), Some(('`', 6)));
        assert_eq!(parse_opening_fence("not a fence"), None);
        assert_eq!(parse_opening_fence("``"), None); // too short
    }

    #[test]
    fn closing_fence_matches() {
        assert!(is_closing_fence("```", '`', 3));
        assert!(is_closing_fence("~~~~", '~', 3));
        assert!(!is_closing_fence("```rust", '`', 3)); // trailing text
        assert!(!is_closing_fence("``", '`', 3)); // too short
    }

    #[test]
    fn count_double_dollar_basic() {
        assert_eq!(count_unescaped_double_dollar("$$"), 1);
        assert_eq!(count_unescaped_double_dollar("$$x$$"), 2);
        assert_eq!(count_unescaped_double_dollar("\\$$"), 0);
        assert_eq!(count_unescaped_double_dollar("$"), 0);
    }

    #[test]
    fn heading_detection() {
        assert!(is_heading_line("# Hello"));
        assert!(is_heading_line("###### Deep"));
        assert!(!is_heading_line("####### Too many"));
        assert!(!is_heading_line("#NoSpace"));
        assert!(!is_heading_line("Not a heading"));
    }

    #[test]
    fn thematic_break_detection() {
        assert!(is_thematic_break_line("---"));
        assert!(is_thematic_break_line("***"));
        assert!(is_thematic_break_line("___"));
        assert!(is_thematic_break_line("- - -"));
        assert!(!is_thematic_break_line("--"));
        assert!(!is_thematic_break_line("- * -"));
    }

    #[test]
    fn ordered_list_detection() {
        assert!(looks_like_ordered_list_item("1. item"));
        assert!(looks_like_ordered_list_item("10) item"));
        assert!(!looks_like_ordered_list_item("1.item"));
        assert!(!looks_like_ordered_list_item("not a list"));
    }

    #[test]
    fn infer_line_kind() {
        assert_eq!(infer_markdown_line_kind("# Heading"), MarkdownBlockKind::Heading);
        assert_eq!(infer_markdown_line_kind("> quote"), MarkdownBlockKind::BlockQuote);
        assert_eq!(infer_markdown_line_kind("- item"), MarkdownBlockKind::List);
        assert_eq!(infer_markdown_line_kind("1. item"), MarkdownBlockKind::List);
        assert_eq!(infer_markdown_line_kind("---"), MarkdownBlockKind::Rule);
        assert_eq!(infer_markdown_line_kind("<div>"), MarkdownBlockKind::HtmlBlock);
        assert_eq!(infer_markdown_line_kind("just text"), MarkdownBlockKind::Paragraph);
    }
}
