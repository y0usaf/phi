//! Prepared multi-section frame (port of jcode's `PreparedChatFrame`).
//!
//! Phi doesn't use ratatui; styled text is `String` with embedded ANSI escape
//! sequences.  The prepared frame segments pre-rendered content into sections
//! (Body, Header, Reasoning, Streaming, InlineImages) so that `scene.rs` can
//! lazily materialize only the visible row range each frame instead of
//! re-rendering the entire live region.

use std::sync::Arc;

// ---------------------------------------------------------------------------
// Section kinds
// ---------------------------------------------------------------------------

/// Identifies the semantic role of a prepared section.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreparedSectionKind {
    /// Main transcript body (committed scrollback + finalized messages).
    Body,
    /// Header / title block above the body.
    Header,
    /// Retained or collapsing reasoning trace.
    Reasoning,
    /// Currently streaming (un-finalized) output.
    Streaming,
    /// Inline images rendered in the transcript flow.
    InlineImages,
}

// ---------------------------------------------------------------------------
// Prepared section
// ---------------------------------------------------------------------------

/// A single pre-rendered section of the chat frame.
#[derive(Clone)]
pub struct PreparedSection {
    pub kind: PreparedSectionKind,
    /// Pre-rendered ANSI-styled lines for this section.
    pub lines: Arc<Vec<String>>,
    /// Absolute start line index within the assembled frame.
    pub line_start: usize,
}

// ---------------------------------------------------------------------------
// Prepared chat frame
// ---------------------------------------------------------------------------

/// A multi-section frame whose sections are pre-rendered once and reused across
/// frames.  Callers slice only the visible rows via [`materialize_line_slice`].
#[derive(Clone)]
pub struct PreparedChatFrame {
    pub sections: Vec<PreparedSection>,
    pub total_lines: usize,
}

impl PreparedChatFrame {
    /// Build a frame from a single section of pre-rendered lines.
    pub fn from_single(kind: PreparedSectionKind, lines: Arc<Vec<String>>) -> Self {
        Self::from_sections(vec![(kind, lines)])
    }

    /// Build a frame from ordered sections.  Empty sections are skipped.
    pub fn from_sections(sections: Vec<(PreparedSectionKind, Arc<Vec<String>>)>) -> Self {
        let mut prepared = Vec::new();
        let mut line_start = 0usize;

        for (kind, lines) in sections {
            if lines.is_empty() {
                continue;
            }
            prepared.push(PreparedSection {
                kind,
                lines,
                line_start,
            });
            line_start += prepared.last().unwrap().lines.len();
        }

        Self {
            sections: prepared,
            total_lines: line_start,
        }
    }

    /// Total number of rendered lines across all sections.
    pub fn total_lines(&self) -> usize {
        self.total_lines
    }

    /// Check whether the visible range `[start, end)` intersects a section of
    /// the given kind.
    pub fn visible_intersects_section(
        &self,
        kind: PreparedSectionKind,
        start: usize,
        end: usize,
    ) -> bool {
        if end <= start {
            return false;
        }
        self.sections.iter().any(|section| {
            if section.kind != kind {
                return false;
            }
            let s = section.line_start;
            let e = s + section.lines.len();
            s < end && start < e
        })
    }

    /// Find the section containing `abs_line` and return a reference to it
    /// along with the local index within that section.
    fn line_section(&self, abs_line: usize) -> Option<(&PreparedSection, usize)> {
        self.sections.iter().find_map(|section| {
            let local = abs_line.checked_sub(section.line_start)?;
            (local < section.lines.len()).then_some((section, local))
        })
    }

    /// Fetch a single line by absolute index.
    pub fn line(&self, abs_line: usize) -> Option<&str> {
        let (section, local) = self.line_section(abs_line)?;
        section.lines.get(local).map(String::as_str)
    }

    /// Lazily materialize the range `[start, end)` as a `Vec<String>`, copying
    /// only the visible rows from the pre-rendered sections.
    pub fn materialize_line_slice(&self, start: usize, end: usize) -> Vec<String> {
        let end = end.min(self.total_lines);
        if start >= end {
            return Vec::new();
        }

        let mut out = Vec::with_capacity(end - start);
        for section in &self.sections {
            let s = section.line_start;
            let e = s + section.lines.len();
            let overlap_start = start.max(s);
            let overlap_end = end.min(e);
            if overlap_start >= overlap_end {
                continue;
            }
            let lo = overlap_start - s;
            let hi = overlap_end - s;
            out.extend_from_slice(&section.lines[lo..hi]);
        }
        out
    }

    /// Materialize every line in the frame.
    pub fn materialize_all_lines(&self) -> Vec<String> {
        self.materialize_line_slice(0, self.total_lines)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arc(lines: &[&str]) -> Arc<Vec<String>> {
        Arc::new(lines.iter().map(|&s| s.to_owned()).collect())
    }

    #[test]
    fn from_single_counts_lines() {
        let frame = PreparedChatFrame::from_single(
            PreparedSectionKind::Body,
            arc(&["a", "b", "c"]),
        );
        assert_eq!(frame.total_lines(), 3);
        assert_eq!(frame.sections.len(), 1);
        assert_eq!(frame.sections[0].line_start, 0);
    }

    #[test]
    fn from_sections_skips_empty() {
        let frame = PreparedChatFrame::from_sections(vec![
            (PreparedSectionKind::Header, arc(&["h"])),
            (PreparedSectionKind::Body, Arc::new(Vec::new())),
            (PreparedSectionKind::Streaming, arc(&["s1", "s2"])),
        ]);
        assert_eq!(frame.sections.len(), 2);
        assert_eq!(frame.total_lines(), 3);
        assert_eq!(frame.sections[0].kind, PreparedSectionKind::Header);
        assert_eq!(frame.sections[1].kind, PreparedSectionKind::Streaming);
        assert_eq!(frame.sections[1].line_start, 1);
    }

    #[test]
    fn materialize_slice_full() {
        let frame = PreparedChatFrame::from_sections(vec![
            (PreparedSectionKind::Body, arc(&["a", "b"])),
            (PreparedSectionKind::Streaming, arc(&["c", "d", "e"])),
        ]);
        let all = frame.materialize_all_lines();
        assert_eq!(all, vec!["a".to_string(), "b".into(), "c".into(), "d".into(), "e".into()]);
    }

    #[test]
    fn materialize_slice_partial_crossing_sections() {
        let frame = PreparedChatFrame::from_sections(vec![
            (PreparedSectionKind::Body, arc(&["a", "b", "c"])),
            (PreparedSectionKind::Streaming, arc(&["d", "e"])),
        ]);
        // Slice [2, 4) → "c", "d"
        let slice = frame.materialize_line_slice(2, 4);
        assert_eq!(slice, vec!["c".to_string(), "d".into()]);
    }

    #[test]
    fn materialize_slice_empty_range() {
        let frame = PreparedChatFrame::from_single(PreparedSectionKind::Body, arc(&["a"]));
        assert!(frame.materialize_line_slice(0, 0).is_empty());
        assert!(frame.materialize_line_slice(1, 0).is_empty());
    }

    #[test]
    fn visible_intersects_section_works() {
        let frame = PreparedChatFrame::from_sections(vec![
            (PreparedSectionKind::Header, arc(&["h1", "h2"])),
            (PreparedSectionKind::Body, arc(&["b1", "b2", "b3"])),
        ]);
        // Body occupies [2, 5).
        assert!(frame.visible_intersects_section(PreparedSectionKind::Body, 3, 4));
        assert!(frame.visible_intersects_section(PreparedSectionKind::Header, 1, 3));
        // Streaming section doesn't exist.
        assert!(!frame.visible_intersects_section(PreparedSectionKind::Streaming, 0, 10));
        // Zero-length range never intersects.
        assert!(!frame.visible_intersects_section(PreparedSectionKind::Body, 3, 3));
    }

    #[test]
    fn line_lookup() {
        let frame = PreparedChatFrame::from_sections(vec![
            (PreparedSectionKind::Body, arc(&["a", "b"])),
            (PreparedSectionKind::InlineImages, arc(&["img"])),
        ]);
        assert_eq!(frame.line(0), Some("a"));
        assert_eq!(frame.line(1), Some("b"));
        assert_eq!(frame.line(2), Some("img"));
        assert_eq!(frame.line(3), None);
    }
}
