//! Message-boundary prefix reuse: when the transcript body changes, find the
//! longest prefix of message blocks whose content hasn't changed (by content
//! hash) and only re-render the changed tail.
//!
//! This is a port of jcode's message-boundary prefix-reuse concept, adapted
//! to phi's transcript types (`BlockSpec`, `Frame`).  The cached body is a
//! flat `Vec<String>` of ANSI-styled lines — exactly what
//! `Transcript::render` produces in `Frame::full`.  Message boundaries are
//! detected at blank-line separators between rendered blocks.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use phi_session::transcript::{BlockSpec, Frame};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Check whether a rendered line is blank (whitespace-only or empty).
fn is_blank(line: &str) -> bool {
    line.chars().all(char::is_whitespace)
}

/// Hash a slice of rendered lines into a stable `u64`.
fn hash_lines(lines: &[String]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for line in lines {
        line.hash(&mut hasher);
    }
    hasher.finish()
}

// ---------------------------------------------------------------------------
// Message boundaries
// ---------------------------------------------------------------------------

/// A message boundary in the flat rendered body: the line range and content
/// hash of a single message block (the content between blank-line
/// separators).
#[derive(Debug, Clone, PartialEq, Eq)]
struct MessageBoundary {
    /// Starting line index (inclusive) in the flat body.
    start: usize,
    /// Ending line index (exclusive) in the flat body.
    end: usize,
    /// Content hash of this block's lines.
    hash: u64,
}

/// Find message boundaries in a flat body.  A new message block starts at the
/// first content line after a blank separator (or at index 0).  Blank
/// separator lines themselves are not part of any block.
fn find_boundaries(lines: &[String]) -> Vec<MessageBoundary> {
    let mut boundaries = Vec::new();
    let mut block_start: Option<usize> = None;

    for (i, line) in lines.iter().enumerate() {
        if is_blank(line) {
            // Close the current block.
            if let Some(start) = block_start.take() {
                if start < i {
                    boundaries.push(MessageBoundary {
                        start,
                        end: i,
                        hash: hash_lines(&lines[start..i]),
                    });
                }
            }
        } else if block_start.is_none() {
            block_start = Some(i);
        }
    }

    // Flush trailing block.
    if let Some(start) = block_start {
        let end = lines.len();
        if start < end {
            boundaries.push(MessageBoundary {
                start,
                end,
                hash: hash_lines(&lines[start..end]),
            });
        }
    }

    boundaries
}

// ---------------------------------------------------------------------------
// Prefix-reuse result
// ---------------------------------------------------------------------------

/// Result of a prefix-reuse diff between a cached body and a new body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefixReuse {
    /// Number of leading lines that are unchanged — these can be reused from
    /// the cached body without re-rendering.
    pub reused_lines: usize,
    /// The changed tail that must be rendered.
    pub tail: Vec<String>,
}

impl PrefixReuse {
    /// Everything is new — nothing can be reused.
    fn all_new(lines: Vec<String>) -> Self {
        Self {
            reused_lines: 0,
            tail: lines,
        }
    }
}

// ---------------------------------------------------------------------------
// Cache
// ---------------------------------------------------------------------------

/// Cached prefix-reuse state from the previous render.
///
/// Stores the full rendered body and pre-computed message boundaries so that
/// `diff()` only hashes the *new* body and compares block hashes against the
/// cached boundaries — O(new_body_len) per frame.
#[derive(Debug, Clone, Default)]
pub struct PrefixReuseCache {
    /// The full rendered body from the previous frame.
    body: Vec<String>,
    /// Pre-computed boundaries for `body`.
    boundaries: Vec<MessageBoundary>,
}

impl PrefixReuseCache {
    /// Create an empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the cache is empty (no previous frame stored).
    pub fn is_empty(&self) -> bool {
        self.body.is_empty()
    }

    /// The previous frame's full body.
    pub fn cached_body(&self) -> &[String] {
        &self.body
    }

    /// Diff the new body against the cached body.  Finds the longest matching
    /// prefix of message blocks (sections separated by blank lines) by content
    /// hash, and returns the number of reusable lines and the changed tail.
    ///
    /// The new body is typically `Frame::full` from `Transcript::render`.
    pub fn diff(&self, new_body: &[String]) -> PrefixReuse {
        if self.body.is_empty() {
            return PrefixReuse::all_new(new_body.to_vec());
        }
        if new_body.is_empty() {
            return PrefixReuse::all_new(Vec::new());
        }

        let new_bounds = find_boundaries(new_body);

        // Find the longest matching prefix of message blocks by hash.
        let mut reused_blocks = 0usize;
        for (old_b, new_b) in self.boundaries.iter().zip(new_bounds.iter()) {
            if old_b.hash == new_b.hash {
                reused_blocks += 1;
            } else {
                break;
            }
        }

        if reused_blocks == 0 {
            return PrefixReuse::all_new(new_body.to_vec());
        }

        // Reuse up to the end of the last matching block, plus any trailing
        // blank separator lines that follow (they haven't changed either).
        let mut reuse_end = new_bounds[reused_blocks - 1].end;
        while reuse_end < new_body.len() && is_blank(&new_body[reuse_end]) {
            reuse_end += 1;
        }
        let tail = new_body[reuse_end..].to_vec();
        PrefixReuse {
            reused_lines: reuse_end,
            tail,
        }
    }

    /// Update the cache with the newly rendered body.
    pub fn commit(&mut self, body: Vec<String>) {
        self.boundaries = find_boundaries(&body);
        self.body = body;
    }

    /// Clear the cache (e.g., on session switch).
    pub fn reset(&mut self) {
        self.body.clear();
        self.boundaries.clear();
    }

    /// Convenience: diff against a `Frame`'s `full` field.
    pub fn diff_frame(&self, frame: &Frame) -> PrefixReuse {
        self.diff(&frame.full)
    }

    /// Convenience: commit a `Frame`'s `full` field.
    pub fn commit_frame(&mut self, frame: &Frame) {
        self.commit(frame.full.clone());
    }
}

// ---------------------------------------------------------------------------
// Block-level hashing (finer-grained diffing on BlockSpec)
// ---------------------------------------------------------------------------

/// Hash the rendered content of a single `BlockSpec`.
pub fn hash_block(block: &BlockSpec<'_>) -> u64 {
    hash_lines(block.lines)
}

/// Compute block-level hashes for a slice of `BlockSpec`s.
pub fn hash_blocks<'a>(blocks: &[BlockSpec<'a>]) -> Vec<(&'a str, u64)> {
    blocks.iter().map(|b| (b.key, hash_block(b))).collect()
}

/// Given old block hashes and new blocks, find the index of the first changed
/// block (the "reuse point").  Blocks before this index can be reused from
/// cache; blocks from this index onward must be re-rendered.
///
/// Returns `new_blocks.len()` if nothing changed (all overlapping blocks
/// match).  Returns `old_hashes.len()` if new blocks were appended.
pub fn first_changed_block(
    old_hashes: &[(&str, u64)],
    new_blocks: &[BlockSpec<'_>],
) -> usize {
    let new_hashes = hash_blocks(new_blocks);

    for (i, ((old_key, old_hash), (new_key, new_hash))) in
        old_hashes.iter().zip(new_hashes.iter()).enumerate()
    {
        if old_key != new_key || old_hash != new_hash {
            return i;
        }
    }

    // All overlapping blocks matched.  If new has more blocks, those are new;
    // otherwise nothing changed.
    old_hashes.len().min(new_blocks.len())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn body(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn find_boundaries_splits_at_blank_lines() {
        let lines = body(&["line1", "line2", "", "line3", "", "line4"]);
        let bounds = find_boundaries(&lines);
        assert_eq!(bounds.len(), 3);
        assert_eq!(bounds[0].start, 0);
        assert_eq!(bounds[0].end, 2);
        assert_eq!(bounds[1].start, 3);
        assert_eq!(bounds[1].end, 4);
        assert_eq!(bounds[2].start, 5);
        assert_eq!(bounds[2].end, 6);
    }

    #[test]
    fn find_boundaries_handles_leading_and_trailing_blanks() {
        let lines = body(&["", "msg1", "", "msg2", ""]);
        let bounds = find_boundaries(&lines);
        assert_eq!(bounds.len(), 2);
        assert_eq!(bounds[0].start, 1);
        assert_eq!(bounds[0].end, 2);
        assert_eq!(bounds[1].start, 3);
        assert_eq!(bounds[1].end, 4);
    }

    #[test]
    fn diff_all_new_when_cache_empty() {
        let cache = PrefixReuseCache::new();
        let result = cache.diff(&body(&["hello"]));
        assert_eq!(result.reused_lines, 0);
        assert_eq!(result.tail, body(&["hello"]));
    }

    #[test]
    fn diff_identical_body_reuses_all() {
        let mut cache = PrefixReuseCache::new();
        let b = body(&["msg1", "", "msg2"]);
        cache.commit(b.clone());
        let result = cache.diff(&b);
        assert_eq!(result.reused_lines, 3);
        assert!(result.tail.is_empty());
    }

    #[test]
    fn diff_changing_tail_returns_tail() {
        let mut cache = PrefixReuseCache::new();
        cache.commit(body(&["msg1", "", "msg2"]));

        let new = body(&["msg1", "", "msg2_changed"]);
        let result = cache.diff(&new);
        assert_eq!(result.reused_lines, 2);
        assert_eq!(result.tail, body(&["msg2_changed"]));
    }

    #[test]
    fn diff_appending_block() {
        let mut cache = PrefixReuseCache::new();
        cache.commit(body(&["msg1", "", "msg2"]));

        let new = body(&["msg1", "", "msg2", "", "msg3"]);
        let result = cache.diff(&new);
        assert_eq!(result.reused_lines, 4);
        assert_eq!(result.tail, body(&["msg3"]));
    }

    #[test]
    fn diff_changing_first_block_reuses_nothing() {
        let mut cache = PrefixReuseCache::new();
        cache.commit(body(&["msg1", "", "msg2"]));

        let new = body(&["msg1_changed", "", "msg2"]);
        let result = cache.diff(&new);
        assert_eq!(result.reused_lines, 0);
        assert_eq!(result.tail, new);
    }

    #[test]
    fn diff_shrinking_body() {
        let mut cache = PrefixReuseCache::new();
        cache.commit(body(&["msg1", "", "msg2", "", "msg3"]));

        let new = body(&["msg1", "", "msg2"]);
        let result = cache.diff(&new);
        assert_eq!(result.reused_lines, 3);
        assert!(result.tail.is_empty());
    }

    #[test]
    fn diff_frame_works() {
        let mut cache = PrefixReuseCache::new();
        let frame = Frame {
            full: body(&["hello"]),
            ..Default::default()
        };
        cache.commit_frame(&frame);

        let new_frame = Frame {
            full: body(&["hello", "", "world"]),
            ..Default::default()
        };
        let result = cache.diff_frame(&new_frame);
        assert_eq!(result.reused_lines, 2);
        assert_eq!(result.tail, body(&["world"]));
    }

    #[test]
    fn reset_clears_cache() {
        let mut cache = PrefixReuseCache::new();
        cache.commit(body(&["msg1"]));
        assert!(!cache.is_empty());
        cache.reset();
        assert!(cache.is_empty());

        let result = cache.diff(&body(&["msg1"]));
        assert_eq!(result.reused_lines, 0);
    }

    fn make_blocks(specs: &[(&str, &str)]) -> (Vec<String>, Vec<String>, Vec<BlockSpec<'static>>) {
        // We can't produce 'static references from owned data, so we leak
        // intentionally in tests.
        let mut all_lines = Vec::new();
        let mut blocks = Vec::new();
        for (key, content) in specs {
            let mut lines = Vec::new();
            for part in content.split('\n') {
                lines.push(part.to_string());
            }
            let leaked: &'static [String] = Box::leak(lines.into_boxed_slice());
            all_lines.extend(leaked.iter().cloned());
            blocks.push(BlockSpec {
                key: Box::leak((*key).to_string().into_boxed_str()),
                lines: leaked,
                finalized: true,
                max_height: None,
            });
        }
        // We return all_lines twice: once for reference, once for hashing.
        (all_lines.clone(), all_lines, blocks)
    }

    #[test]
    fn first_changed_block_detects_change() {
        let (_a, _b, blocks) = make_blocks(&[("a", "line1"), ("b", "line2")]);
        let hashes = hash_blocks(&blocks);

        // Same blocks → nothing changed.
        assert_eq!(first_changed_block(&hashes, &blocks), 2);

        // Change second block's content.
        let (_a2, _b2, blocks2) = make_blocks(&[("a", "line1"), ("b", "changed")]);
        assert_eq!(first_changed_block(&hashes, &blocks2), 1);

        // Change second block's key.
        let (_a3, _b3, blocks3) = make_blocks(&[("a", "line1"), ("c", "line2")]);
        assert_eq!(first_changed_block(&hashes, &blocks3), 1);
    }

    #[test]
    fn first_changed_block_appended() {
        let (_a, _b, blocks) = make_blocks(&[("a", "line1")]);
        let hashes = hash_blocks(&blocks);

        let (_a2, _b2, blocks2) = make_blocks(&[("a", "line1"), ("b", "line2")]);
        // First block matches, second is new → change at index 1.
        assert_eq!(first_changed_block(&hashes, &blocks2), 1);
    }

    #[test]
    fn first_changed_block_removed() {
        let (_a, _b, blocks) = make_blocks(&[("a", "line1"), ("b", "line2")]);
        let hashes = hash_blocks(&blocks);

        let (_a2, _b2, blocks2) = make_blocks(&[("a", "line1")]);
        // First block matches, second was removed → 1 (the overlap length).
        assert_eq!(first_changed_block(&hashes, &blocks2), 1);
    }

    #[test]
    fn diff_reuses_after_commit_cycle() {
        let mut cache = PrefixReuseCache::new();

        // First render.
        let body1 = body(&["msg1", "", "msg2"]);
        cache.commit(body1.clone());

        // Second render: tail changes.
        let body2 = body(&["msg1", "", "msg2_v2"]);
        let result = cache.diff(&body2);
        assert_eq!(result.reused_lines, 2);
        assert_eq!(result.tail, body(&["msg2_v2"]));

        // Commit second render.
        cache.commit(body2.clone());

        // Third render: same as second → all reused.
        let body3 = body2.clone();
        let result = cache.diff(&body3);
        assert_eq!(result.reused_lines, 3);
        assert!(result.tail.is_empty());
    }
}
