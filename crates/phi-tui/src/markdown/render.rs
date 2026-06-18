use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use unicode_width::UnicodeWidthStr;

use super::constants::*;
use super::highlight::{highlight_code, strip_ansi};

/// Color a diff-preview line: additions green, deletions red, hunk headers
/// violet. File headers (`+++`/`---`) and context lines return None.
pub fn highlight_diff_line(line: &str) -> Option<String> {
    if line.starts_with("@@") {
        Some(format!("{REPL_FENCE}{line}{RESET}"))
    } else if line.starts_with('+') && !line.starts_with("+++") {
        Some(format!("{DIFF_ADD}{line}{RESET}"))
    } else if line.starts_with('-') && !line.starts_with("---") {
        Some(format!("{DIFF_DEL}{line}{RESET}"))
    } else {
        None
    }
}

#[derive(Clone, Copy, Default)]
pub(super) struct Style {
    bold: bool,
    italic: bool,
    code: bool,
    dim: bool,
    link: bool,
    heading: bool,
    subhead: bool,
    accent: Option<&'static str>,
}

impl Style {
    fn prefix(&self) -> String {
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
            s.push_str(SUBHEAD);
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
pub fn render_markdown(text: &str, width: usize) -> Vec<String> {
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
        link: None,
        started: false,
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

pub(super) struct Renderer {
    width: usize,
    lines: Vec<String>,
    current: String,
    col: usize,
    indent: String,
    style: Style,
    list_stack: Vec<ListLevel>,
    quote_depth: usize,
    in_code_block: bool,
    /// Language of the current fenced code block (drives highlighting and
    /// the closing fence line).
    code_lang: Option<String>,
    /// Current link target and whether the link text equals it (autolinks
    /// skip the trailing `(url)`).
    link: Option<(String, bool)>,
    started: bool,
}

impl Renderer {
    fn run(&mut self, parser: Parser) {
        for event in parser {
            match event {
                Event::Start(tag) => self.start_tag(tag),
                Event::End(tag) => self.end_tag(tag),
                Event::Text(t) => {
                    if let Some((url, matches)) = &mut self.link
                        && t.as_ref() == url
                    {
                        *matches = true;
                    }
                    if self.in_code_block {
                        self.push_code_text(&t);
                    } else {
                        self.push_words(&t);
                    }
                }
                Event::Code(t) => {
                    let saved = self.style;
                    self.style.code = true;
                    self.push_words(&t);
                    self.style = saved;
                }
                Event::SoftBreak => self.push_words(" "),
                Event::HardBreak => self.flush_line(),
                Event::Rule => {
                    self.blank_line();
                    let line = format!("{DIM}{}{RESET}", "─".repeat(self.width));
                    self.lines.push(line);
                }
                Event::TaskListMarker(done) => {
                    let saved = self.style;
                    if done {
                        self.style.accent = Some(TASK_DONE);
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
                if let CodeBlockKind::Fenced(lang) = kind
                    && !lang.is_empty()
                {
                    if lang.trim().eq_ignore_ascii_case("repl") {
                        self.lines.push(format!("{REPL_FENCE}```repl{RESET}"));
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
                self.style.accent = Some(MARKER_ACCENT);
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
                if self
                    .code_lang
                    .as_deref()
                    .is_some_and(|lang| lang.eq_ignore_ascii_case("repl"))
                {
                    self.lines.push(format!("{REPL_FENCE}```{RESET}"));
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
            _ => {}
        }
    }

    fn update_indent(&mut self) {
        let hang: usize = self.list_stack.iter().map(|l| l.hang).sum();
        let mut prefix = format!("{}{}", "│ ".repeat(self.quote_depth), " ".repeat(hang));
        if self.quote_depth > 0 {
            prefix = format!(
                "{QUOTE_BAR}{}{RESET}{}",
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
            let prefix = self.style.prefix();
            if prefix.is_empty() {
                self.current.push_str(word);
            } else {
                self.current.push_str(&prefix);
                self.current.push_str(word);
                self.current.push_str(RESET);
            }
            self.col += w;
        }
    }

    fn push_code_text(&mut self, text: &str) {
        let highlighted = highlight_code(text, self.code_lang.as_deref());
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
            self.lines.push(line);
            self.col = 0;
            self.started = true;
        }
    }
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
