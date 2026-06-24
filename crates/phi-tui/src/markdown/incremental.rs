//! Incremental markdown renderer with caching.
//!
//! Caches rendered lines from the previous frame and skips the parser
//! entirely when the input text (by hash) and width are unchanged.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use super::constants::MarkdownPalette;
use super::render::render_markdown;

/// Incremental markdown renderer that caches rendered output keyed by text
/// hash + width, skipping the parser on unchanged input.
pub struct IncrementalMarkdownRenderer {
    /// Hash of the last rendered text.
    last_text_hash: u64,
    /// Width used for the last render.
    last_width: usize,
    /// Previously rendered ANSI-styled lines.
    cached_lines: Vec<String>,
    /// Palette used for rendering.
    palette: MarkdownPalette,
}

impl IncrementalMarkdownRenderer {
    /// Create a new incremental renderer with the default palette.
    pub fn new() -> Self {
        Self {
            last_text_hash: 0,
            last_width: 0,
            cached_lines: Vec::new(),
            palette: MarkdownPalette::default(),
        }
    }

    /// Set the palette, clearing the cache (forces re-render on next call).
    pub fn set_palette(&mut self, palette: MarkdownPalette) {
        if !palette_eq(&self.palette, &palette) {
            self.palette = palette;
            self.reset();
        }
    }

    /// Render markdown to terminal lines, returning the cached result when
    /// the input text hash and width are unchanged from the previous call.
    ///
    /// Fast path: hashes `text` and compares against `last_text_hash`; if it
    /// matches and the width hasn't changed, the parser is skipped entirely.
    pub fn render(&mut self, text: &str, width: usize) -> Vec<String> {
        let hash = hash_text(text);
        if hash == self.last_text_hash && width == self.last_width {
            return self.cached_lines.clone();
        }

        self.cached_lines = render_markdown(text, width, &self.palette);
        self.last_text_hash = hash;
        self.last_width = width;
        self.cached_lines.clone()
    }

    /// Reset the renderer state, clearing the cache.
    pub fn reset(&mut self) {
        self.last_text_hash = 0;
        self.last_width = 0;
        self.cached_lines.clear();
    }
}

impl Default for IncrementalMarkdownRenderer {
    fn default() -> Self {
        Self::new()
    }
}

/// Hash a text slice to a 64-bit key using the default hasher.
fn hash_text(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// Compare two palettes for equality by their ANSI string fields.
/// `syntax_theme` is excluded (it is derived from the other fields).
fn palette_eq(a: &MarkdownPalette, b: &MarkdownPalette) -> bool {
    a.repl_fence == b.repl_fence
        && a.repl_fn == b.repl_fn
        && a.marker_accent == b.marker_accent
        && a.task_done == b.task_done
        && a.quote_bar == b.quote_bar
        && a.subhead == b.subhead
        && a.out_base == b.out_base
        && a.out_err == b.out_err
        && a.out_warn == b.out_warn
        && a.out_ok == b.out_ok
        && a.out_path == b.out_path
        && a.out_num == b.out_num
        && a.out_str == b.out_str
        && a.out_url == b.out_url
        && a.diff_add == b.diff_add
        && a.diff_del == b.diff_del
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_hit_on_unchanged_text() {
        let mut r = IncrementalMarkdownRenderer::new();
        let first = r.render("# Hello\nworld", 80);
        let second = r.render("# Hello\nworld", 80);
        assert_eq!(first, second);
    }

    #[test]
    fn re_renders_on_changed_text() {
        let mut r = IncrementalMarkdownRenderer::new();
        let first = r.render("# Hello", 80);
        let second = r.render("# World", 80);
        assert_ne!(first, second);
    }

    #[test]
    fn re_renders_on_width_change() {
        let mut r = IncrementalMarkdownRenderer::new();
        let wide = r.render("one two three four", 80);
        let narrow = r.render("one two three four", 10);
        assert_ne!(wide.len(), narrow.len());
    }

    #[test]
    fn reset_clears_cache() {
        let mut r = IncrementalMarkdownRenderer::new();
        let _ = r.render("# Hello", 80);
        r.reset();
        assert!(r.cached_lines.is_empty());
        assert_eq!(r.last_text_hash, 0);
    }
}
