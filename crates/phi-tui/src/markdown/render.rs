use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::constants::{BOLD, DIM, ITALIC, RESET, UNDERLINE, CYAN, MarkdownPalette};
use super::highlight::highlight_code_cached;
use super::checkpoint::MarkdownBlockKind;

/// Semantic spacing mode controlling vertical separation between blocks.
///
/// * `Compact` — minimal spacing; headings are followed by a separator blank
///   line but other blocks (paragraphs, lists, code, etc.) are packed tightly.
/// * `Document` — generous spacing; every block is followed by a separator
///   blank line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MarkdownSpacingMode {
    #[default]
    Compact,
    Document,
}

/// Return the number of blank lines to insert after a block of `kind` under
/// the given spacing `mode`.
///
/// Ported from jcode's `spacing_separates_after`. The original jcode function
/// returns a `bool`; this port returns `usize` (0 or 1) so callers can insert
/// the correct number of blank lines directly.
///
/// * **Compact mode** — only headings get a trailing separator; everything
///   else is packed with no inter-block blank line.
/// * **Document mode** — every block type gets a trailing separator blank
///   line.
pub fn spacing_separates_after(kind: MarkdownBlockKind, mode: MarkdownSpacingMode) -> usize {
    match mode {
        MarkdownSpacingMode::Compact => {
            if matches!(kind, MarkdownBlockKind::Heading) {
                1
            } else {
                0
            }
        }
        MarkdownSpacingMode::Document => 1,
    }
}

/// Color a diff-preview line: additions green, deletions red, hunk headers
/// violet. File headers (`+++`/`---`) and context lines return None.
pub fn highlight_diff_line(line: &str, palette: &MarkdownPalette) -> Option<String> {
    if line.starts_with("@@") {
        Some(format!("{}{line}{RESET}", palette.repl_fence))
    } else if line.starts_with('+') && !line.starts_with("+++") {
        Some(format!("{}{line}{RESET}", palette.diff_add))
    } else if line.starts_with('-') && !line.starts_with("---") {
        Some(format!("{}{line}{RESET}", palette.diff_del))
    } else {
        None
    }
}

#[derive(Clone, Copy, Default)]
pub(super) struct Style<'p> {
    bold: bool,
    italic: bool,
    code: bool,
    dim: bool,
    link: bool,
    heading: bool,
    subhead: bool,
    accent: Option<&'p str>,
}

impl<'p> Style<'p> {
    fn prefix(&self, palette: &MarkdownPalette) -> String {
        let mut s = String::new();
        if self.bold {
            s.push_str(BOLD);
        }
        if self.italic {
            s.push_str(ITALIC);
        }
        if self.dim {
            s.push_str(DIM);
        }
        if self.code || self.heading {
            s.push_str(CYAN);
        }
        if self.subhead {
            s.push_str(&palette.subhead);
        }
        if let Some(accent) = self.accent {
            s.push_str(accent);
        }
        if self.link {
            s.push_str(UNDERLINE);
            s.push_str(CYAN);
        }
        s
    }
}

/// Render markdown to terminal lines, word-wrapped to `width` columns.
pub fn render_markdown(text: &str, width: usize, palette: &MarkdownPalette) -> Vec<String> {
    let width = width.max(1);
    let options =
        Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS;
    let parser = Parser::new_ext(text, options);

    let mut r = Renderer {
        width,
        lines: Vec::new(),
        current: String::new(),
        col: 0,
        indent: String::new(),
        style: Style::default(),
        list_stack: Vec::new(),
        quote_depth: 0,
        in_code_block: false,
        code_lang: None,
        code_block_content: String::new(),
        code_block_start_line: 0,
        lazy_viewport: None,
        link: None,
        started: false,
        palette,
        active_prefix: String::new(),
        in_table: false,
        table_rows: Vec::new(),
        table_alignments: Vec::new(),
        table_current_row: Vec::new(),
        table_current_cell: String::new(),
    };
    r.run(parser);
    r.flush_line();
    while r.lines.last().is_some_and(|l| l.is_empty()) {
        r.lines.pop();
    }
    r.lines
}

/// Render markdown with viewport-aware code block highlighting.
///
/// Only code blocks whose line range intersects `[viewport_start,
/// viewport_end)` are syntax-highlighted; off-screen blocks use a cheap
/// dimmed placeholder. All non-code-block elements are rendered identically
/// to [`render_markdown`].
pub fn render_markdown_lazy(
    text: &str,
    width: usize,
    viewport_start: usize,
    viewport_end: usize,
    palette: &MarkdownPalette,
) -> Vec<String> {
    let width = width.max(1);
    let options =
        Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS;
    let parser = Parser::new_ext(text, options);

    let mut r = Renderer {
        width,
        lines: Vec::new(),
        current: String::new(),
        col: 0,
        indent: String::new(),
        style: Style::default(),
        list_stack: Vec::new(),
        quote_depth: 0,
        in_code_block: false,
        code_lang: None,
        code_block_content: String::new(),
        code_block_start_line: 0,
        lazy_viewport: Some((viewport_start, viewport_end)),
        link: None,
        started: false,
        palette,
        active_prefix: String::new(),
        in_table: false,
        table_rows: Vec::new(),
        table_alignments: Vec::new(),
        table_current_row: Vec::new(),
        table_current_cell: String::new(),
    };
    r.run(parser);
    r.flush_line();
    while r.lines.last().is_some_and(|l| l.is_empty()) {
        r.lines.pop();
    }
    r.lines
}

pub(super) struct ListLevel {
    /// Next ordered-list number, or None for bullets.
    next: Option<u64>,
    /// Display width of the current item's marker: wrapped continuations
    /// (and nested lists) hang under the item text, not the marker.
    hang: usize,
}

pub(super) struct Renderer<'p> {
    width: usize,
    lines: Vec<String>,
    current: String,
    col: usize,
    indent: String,
    style: Style<'p>,
    list_stack: Vec<ListLevel>,
    quote_depth: usize,
    in_code_block: bool,
    /// Language of the current fenced code block (drives highlighting and
    /// the closing fence line).
    code_lang: Option<String>,
    /// Accumulated code block content for lazy (viewport-aware) highlighting.
    code_block_content: String,
    /// Line index where the current code block started (for viewport checks).
    code_block_start_line: usize,
    /// When set, only syntax-highlight code blocks intersecting this range;
    /// off-screen blocks get a cheap dimmed placeholder instead.
    lazy_viewport: Option<(usize, usize)>,
    /// Current link target and whether the link text equals it (autolinks
    /// skip the trailing `(url)`).
    link: Option<(String, bool)>,
    started: bool,
    palette: &'p MarkdownPalette,
    /// Currently active ANSI style prefix in `self.current`. Empty means
    /// default (no active styling). Tracked to avoid redundant escape
    /// sequences across consecutive same-styled words (span defragmentation).
    active_prefix: String,
    // --- Table state ---
    in_table: bool,
    table_rows: Vec<Vec<String>>,
    table_alignments: Vec<pulldown_cmark::Alignment>,
    table_current_row: Vec<String>,
    table_current_cell: String,
}

impl<'p> Renderer<'p> {
    fn run(&mut self, parser: Parser) {
        for event in parser {
            match event {
                Event::Start(tag) => self.start_tag(tag),
                Event::End(tag) => self.end_tag(tag),
                Event::Text(t) => {
                    if self.in_table {
                        self.table_current_cell.push_str(&t);
                    } else if self.in_code_block {
                        self.push_code_text(&t);
                    } else {
                        if let Some((url, matches)) = &mut self.link
                            && t.as_ref() == url
                        {
                            *matches = true;
                        }
                        self.push_words(&t);
                    }
                }
                Event::Code(t) => {
                    if self.in_table {
                        self.table_current_cell.push_str(&t);
                    } else {
                        let saved = self.style;
                        self.style.code = true;
                        self.push_words(&t);
                        self.style = saved;
                    }
                }
                Event::SoftBreak => {
                    if self.in_table {
                        self.table_current_cell.push(' ');
                    } else {
                        self.push_words(" ");
                    }
                }
                Event::HardBreak => self.flush_line(),
                Event::Rule => {
                    self.blank_line();
                    let line = format!("{DIM}{}{RESET}", "─".repeat(self.width));
                    self.lines.push(line);
                }
                Event::TaskListMarker(done) => {
                    let saved = self.style;
                    if done {
                        self.style.accent = Some(&self.palette.task_done);
                    } else {
                        self.style.dim = true;
                    }
                    self.push_words(if done { "[x] " } else { "[ ] " });
                    self.style = saved;
                }
                _ => {}
            }
        }
    }

    fn start_tag(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph => self.blank_line(),
            Tag::Heading { level, .. } => {
                self.blank_line();
                self.style.bold = true;
                self.style.heading = level <= HeadingLevel::H2;
                self.style.subhead = level > HeadingLevel::H2;
            }
            Tag::BlockQuote(_) => {
                self.blank_line();
                self.quote_depth += 1;
                self.style.dim = true;
                self.update_indent();
            }
            Tag::CodeBlock(kind) => {
                self.blank_line();
                self.in_code_block = true;
                self.code_lang = None;
                self.code_block_content.clear();
                self.code_block_start_line = self.lines.len();
                if let CodeBlockKind::Fenced(lang) = kind
                    && !lang.is_empty()
                {
                    if lang.trim().eq_ignore_ascii_case("repl") {
                        let fence = &self.palette.repl_fence;
                        self.lines.push(format!("{fence}```repl{RESET}"));
                    } else {
                        self.lines.push(format!("{DIM}\u{258d} {lang}{RESET}"));
                    }
                    self.code_lang = Some(lang.to_string());
                }
            }
            Tag::List(start) => {
                if self.list_stack.is_empty() {
                    self.blank_line();
                } else {
                    self.flush_line();
                }
                self.list_stack.push(ListLevel {
                    next: start,
                    hang: 0,
                });
                self.update_indent();
            }
            Tag::Item => {
                self.flush_line();
                let marker = match self.list_stack.last_mut() {
                    Some(ListLevel { next: Some(n), .. }) => {
                        let m = format!("{n}. ");
                        *n += 1;
                        m
                    }
                    _ => "• ".to_owned(),
                };
                // The marker line starts at the enclosing indent; wrapped
                // continuations hang under the item text.
                if let Some(level) = self.list_stack.last_mut() {
                    level.hang = 0;
                }
                self.update_indent();
                let saved = self.style;
                self.style.accent = Some(&self.palette.marker_accent);
                self.push_words(&marker);
                self.style = saved;
                if let Some(level) = self.list_stack.last_mut() {
                    level.hang = marker.width();
                }
                self.update_indent();
            }
            Tag::Emphasis => self.style.italic = true,
            Tag::Strong => self.style.bold = true,
            Tag::Link { dest_url, .. } => {
                self.style.link = true;
                self.link = Some((dest_url.to_string(), false));
            }
            Tag::Table(alignments) => {
                self.flush_line();
                self.in_table = true;
                self.table_rows = Vec::new();
                self.table_alignments = alignments.to_vec();
                self.table_current_row = Vec::new();
                self.table_current_cell = String::new();
            }
            Tag::TableHead | Tag::TableRow => {
                self.table_current_row = Vec::new();
                self.table_current_cell = String::new();
            }
            Tag::TableCell => {
                if !self.table_current_cell.is_empty() {
                    self.table_current_row
                        .push(std::mem::take(&mut self.table_current_cell));
                }
                self.table_current_cell = String::new();
            }
            _ => {}
        }
    }

    fn end_tag(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.flush_line();
            }
            TagEnd::Heading(level) => {
                self.style.bold = false;
                self.style.heading = false;
                self.style.subhead = false;
                let title_width = self.col.min(self.width);
                self.flush_line();
                if level == HeadingLevel::H1 && title_width > 0 {
                    self.lines
                        .push(format!("{DIM}{}{RESET}", "─".repeat(title_width)));
                }
            }
            TagEnd::BlockQuote(_) => {
                self.quote_depth -= 1;
                self.style.dim = self.quote_depth > 0;
                self.update_indent();
                self.flush_line();
            }
            TagEnd::CodeBlock => {
                if let Some((vp_start, vp_end)) = self.lazy_viewport {
                    // Lazy mode: decide whether to highlight or use placeholder
                    let code_line_count = self.code_block_content.lines().count();
                    let block_range = self
                        .code_block_start_line
                        ..(self.code_block_start_line + code_line_count + 2);
                    let is_visible = ranges_overlap(block_range, vp_start..vp_end);

                    let code_lines = if is_visible {
                        highlight_code_cached(
                            &self.code_block_content,
                            self.code_lang.as_deref(),
                            self.palette,
                        )
                    } else {
                        // Cheap placeholder: dimmed text without highlighting
                        self.code_block_content
                            .lines()
                            .map(|line| format!("{DIM}{line}{RESET}"))
                            .collect::<Vec<_>>()
                    };

                    for line in code_lines {
                        self.lines
                            .push(format!("{DIM}\u{258f}{RESET} {line}{RESET}"));
                    }
                    self.started = true;
                    self.code_block_content.clear();
                }
                if self
                    .code_lang
                    .as_deref()
                    .is_some_and(|lang| lang.eq_ignore_ascii_case("repl"))
                {
                    let fence = &self.palette.repl_fence;
                    self.lines.push(format!("{fence}```{RESET}"));
                }
                self.in_code_block = false;
                self.code_lang = None;
                self.flush_line();
            }
            TagEnd::List(_) => {
                self.list_stack.pop();
                self.update_indent();
                self.flush_line();
            }
            TagEnd::Item => self.flush_line(),
            TagEnd::Emphasis => self.style.italic = false,
            TagEnd::Strong => self.style.bold = false,
            TagEnd::Link => {
                self.style.link = false;
                if let Some((url, matched)) = self.link.take()
                    && !matched
                    && !url.is_empty()
                {
                    let saved = self.style;
                    self.style.dim = true;
                    self.push_words(&format!(" ({url})"));
                    self.style = saved;
                }
            }
            TagEnd::TableCell => {
                if !self.table_current_cell.is_empty() {
                    self.table_current_row
                        .push(std::mem::take(&mut self.table_current_cell));
                }
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                let row = std::mem::take(&mut self.table_current_row);
                if !row.is_empty() {
                    self.table_rows.push(row);
                }
            }
            TagEnd::Table => {
                let rows = std::mem::take(&mut self.table_rows);
                let alignments = std::mem::take(&mut self.table_alignments);
                let table_lines =
                    render_table_with_width(&rows, &alignments, self.width);
                self.lines.extend(table_lines);
                self.in_table = false;
                self.table_current_cell.clear();
                self.started = true;
            }
            _ => {}
        }
    }

    fn update_indent(&mut self) {
        let hang: usize = self.list_stack.iter().map(|l| l.hang).sum();
        let mut prefix = format!("{}{}", "│ ".repeat(self.quote_depth), " ".repeat(hang));
        if self.quote_depth > 0 {
            prefix = format!(
                "{}{}{RESET}{}",
                self.palette.quote_bar,
                "│ ".repeat(self.quote_depth),
                " ".repeat(hang)
            );
        }
        self.indent = prefix;
    }

    fn push_words(&mut self, text: &str) {
        for word in split_words(text) {
            if word == " " {
                if self.col > 0 {
                    self.ensure_line_started();
                    self.current.push(' ');
                    self.col += 1;
                }
                continue;
            }
            let w = word.width();
            if self.col > 0 && self.col + w > self.width {
                self.flush_line();
            }
            self.ensure_line_started();
            let avail = self.width.saturating_sub(self.col);
            if w > avail {
                // Over-wide token (URL, long code): hard-split char by char,
                // preserving style across line boundaries.
                self.establish_style();
                for ch in word.chars() {
                    let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
                    if self.col + cw > self.width && self.col > 0 {
                        self.flush_line();
                        self.ensure_line_started();
                        self.establish_style();
                    }
                    self.current.push(ch);
                    self.col += cw;
                }
            } else {
                self.establish_style();
                self.current.push_str(word);
                self.col += w;
            }
        }
    }

    /// Emit the current style prefix onto `current` when the active style
    /// differs from `active_prefix`, enabling span defragmentation across
    /// consecutive same-styled words.
    fn establish_style(&mut self) {
        let prefix = self.style.prefix(self.palette);
        if prefix != self.active_prefix {
            if !self.active_prefix.is_empty() {
                self.current.push_str(RESET);
            }
            if !prefix.is_empty() {
                self.current.push_str(&prefix);
            }
            self.active_prefix = prefix;
        }
    }

    fn push_code_text(&mut self, text: &str) {
        if self.lazy_viewport.is_some() {
            // Lazy mode: accumulate content; highlight or placeholder at end
            self.code_block_content.push_str(text);
            return;
        }
        let highlighted = highlight_code_cached(text, self.code_lang.as_deref(), self.palette);
        for line in highlighted {
            self.lines
                .push(format!("{DIM}\u{258f}{RESET} {line}{RESET}"));
        }
        self.started = true;
    }

    fn ensure_line_started(&mut self) {
        if self.col == 0 && self.current.is_empty() && !self.indent.is_empty() {
            self.current = self.indent.clone();
            self.col = crate::terminal::visible_width(&self.indent);
        }
    }

    fn blank_line(&mut self) {
        self.flush_line();
        if self.started && self.lines.last().is_some_and(|l| !l.is_empty()) {
            self.lines.push(String::new());
        }
    }

    fn flush_line(&mut self) {
        if !self.current.is_empty() || self.col > 0 {
            let mut line = std::mem::take(&mut self.current);
            while line.ends_with(' ') {
                line.pop();
            }
            // Close any active styling at end of line.
            if !self.active_prefix.is_empty() {
                line.push_str(RESET);
                self.active_prefix.clear();
            }
            self.lines.push(line);
            self.col = 0;
            self.started = true;
        }
    }
}

/// Check if two half-open ranges overlap.
fn ranges_overlap(a: std::ops::Range<usize>, b: std::ops::Range<usize>) -> bool {
    a.start < b.end && b.start < a.end
}

/// Split text into words and single-space separators (runs collapse).
pub(super) fn split_words(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let trimmed = rest.trim_start_matches([' ', '\t', '\n']);
        if trimmed.len() < rest.len() {
            out.push(" ");
            rest = trimmed;
            continue;
        }
        let end = rest.find([' ', '\t', '\n']).unwrap_or(rest.len());
        out.push(&rest[..end]);
        rest = &rest[end..];
    }
    out
}

/// Break a single long word into chunks no wider than `width`.
fn wrap_long_table_word(word: &str, width: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;

    for ch in word.chars() {
        let ch_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if current_width + ch_width > width && !current.is_empty() {
            chunks.push(std::mem::take(&mut current));
            current_width = 0;
        }
        current.push(ch);
        current_width += ch_width;
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    chunks
}

/// Word-wrap a cell to fit within `width` columns.
fn wrap_table_cell(cell: &str, width: usize) -> Vec<String> {
    if width == 0 || cell.is_empty() {
        return vec![String::new()];
    }

    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;

    for word in cell.split(' ') {
        let word_width = UnicodeWidthStr::width(word);
        let space_width = usize::from(!current.is_empty());

        if !current.is_empty() && current_width + space_width + word_width <= width {
            current.push(' ');
            current.push_str(word);
            current_width += space_width + word_width;
            continue;
        }

        if !current.is_empty() {
            lines.push(std::mem::take(&mut current));
            current_width = 0;
        }

        if word_width <= width {
            current.push_str(word);
            current_width = word_width;
        } else {
            for chunk in wrap_long_table_word(word, width) {
                if UnicodeWidthStr::width(chunk.as_str()) == width {
                    lines.push(chunk);
                } else {
                    current = chunk;
                    current_width = UnicodeWidthStr::width(current.as_str());
                }
            }
        }
    }

    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }

    lines
}

/// Render a markdown table as ANSI-styled `Vec<String>`, ported from jcode's
/// `render_table` + `render_table_with_width`.  Uses `BOLD`/`RESET` for the
/// header row, "│" separators, and "─┼─" for the separator line.
pub(super) fn render_table_with_width(
    rows: &[Vec<String>],
    _alignments: &[pulldown_cmark::Alignment],
    width: usize,
) -> Vec<String> {
    if rows.is_empty() {
        return vec![];
    }

    let mut lines = Vec::new();

    // Calculate column widths
    let num_cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    let mut col_widths: Vec<usize> = vec![0; num_cols];

    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < col_widths.len() {
                col_widths[i] = col_widths[i].max(UnicodeWidthStr::width(cell.as_str()));
            }
        }
    }

    // Apply max width constraint
    let separator_space = if num_cols > 1 { (num_cols - 1) * 3 } else { 0 };
    let available = width.saturating_sub(separator_space);

    if available > 0 && num_cols > 0 {
        let total_width: usize = col_widths.iter().sum();
        if total_width > available {
            let min_col_width = (available / num_cols).clamp(1, 5);
            for w in &mut col_widths {
                *w = (*w).max(min_col_width);
            }

            while col_widths.iter().sum::<usize>() > available {
                if let Some((idx, _)) = col_widths
                    .iter()
                    .enumerate()
                    .filter(|(_, w)| **w > min_col_width)
                    .max_by_key(|(_, w)| *w)
                {
                    col_widths[idx] -= 1;
                } else {
                    break;
                }
            }
        }
    }

    for (row_idx, row) in rows.iter().enumerate() {
        let wrapped_cells: Vec<Vec<String>> = row
            .iter()
            .enumerate()
            .map(|(i, cell)| {
                let col_width = col_widths
                    .get(i)
                    .copied()
                    .unwrap_or_else(|| UnicodeWidthStr::width(cell.as_str()));
                wrap_table_cell(cell, col_width)
            })
            .collect();
        let physical_rows = wrapped_cells
            .iter()
            .map(|cell_lines| cell_lines.len())
            .max()
            .unwrap_or(1);

        for physical_idx in 0..physical_rows {
            let mut line = String::new();
            let is_header = row_idx == 0;

            if is_header {
                line.push_str(BOLD);
            }

            for (i, cell_lines) in wrapped_cells.iter().enumerate() {
                let display_text = cell_lines
                    .get(physical_idx)
                    .map(String::as_str)
                    .unwrap_or("");
                let col_width = col_widths
                    .get(i)
                    .copied()
                    .unwrap_or_else(|| UnicodeWidthStr::width(display_text));
                let text_width = UnicodeWidthStr::width(display_text);
                let pad = col_width.saturating_sub(text_width);
                let padded = format!("{}{}", display_text, " ".repeat(pad));

                if i > 0 {
                    line.push('│');
                }
                line.push_str(&padded);
            }

            line.push_str(RESET);
            lines.push(line);
        }

        if row_idx == 0 {
            let separator: String = col_widths
                .iter()
                .map(|&w| "─".repeat(w))
                .collect::<Vec<_>>()
                .join("─┼─");
            lines.push(separator);
        }
    }

    lines
}

/// Type of structured block that can be centered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuredBlockType {
    Code,
    Math,
    Table,
}

/// Compute the left padding needed to center a structured block within the
/// available terminal width.
///
/// Takes the block type (for future per-type policy), the block's display
/// width, and the total terminal width.  Returns the number of leading
/// spaces to prepend to each line of the block.  If the block is wider than
/// or equal to the terminal, returns 0 (no padding).
pub fn center_structured_block_ranges(
    _block_type: StructuredBlockType,
    block_width: usize,
    terminal_width: usize,
) -> usize {
    if terminal_width == 0 || block_width >= terminal_width {
        return 0;
    }
    (terminal_width - block_width) / 2
}

/// Apply horizontal centering padding to a slice of rendered ANSI lines.
///
/// Computes the block width as the widest non-empty line (ANSI-aware), then
/// prepends `center_structured_block_ranges(...)` spaces to each non-empty
/// line.  Blank lines are skipped to avoid trailing-whitespace artefacts.
pub fn apply_centering(
    lines: &mut [String],
    block_type: StructuredBlockType,
    terminal_width: usize,
) {
    if terminal_width == 0 || lines.is_empty() {
        return;
    }

    let block_width = lines
        .iter()
        .filter(|l| !l.is_empty())
        .map(|l| crate::terminal::visible_width(l))
        .max()
        .unwrap_or(0);

    let pad = center_structured_block_ranges(block_type, block_width, terminal_width);
    if pad == 0 {
        return;
    }

    let pad_str = " ".repeat(pad);
    for line in lines.iter_mut() {
        if !line.is_empty() {
            line.insert_str(0, &pad_str);
        }
    }
}
