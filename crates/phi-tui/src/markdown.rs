//! Markdown → ANSI-styled terminal lines with word wrapping.

use std::sync::LazyLock;

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::as_24_bit_terminal_escaped;
use unicode_width::UnicodeWidthStr;

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const ITALIC: &str = "\x1b[3m";
const UNDERLINE: &str = "\x1b[4m";
const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";

static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
static THEME: LazyLock<Theme> = LazyLock::new(|| {
    ThemeSet::load_defaults()
        .themes
        .remove("base16-ocean.dark")
        .expect("bundled theme")
});

#[derive(Clone, Copy, Default)]
struct Style {
    bold: bool,
    italic: bool,
    code: bool,
    dim: bool,
    link: bool,
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
        if self.code {
            s.push_str(CYAN);
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
    let width = width.max(20);
    let options = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES;
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

struct ListLevel {
    /// Next ordered-list number, or None for bullets.
    next: Option<u64>,
    /// Display width of the current item's marker: wrapped continuations
    /// (and nested lists) hang under the item text, not the marker.
    hang: usize,
}

struct Renderer {
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
                    let line = format!("{DIM}{}{RESET}", "─".repeat(self.width.min(40)));
                    self.lines.push(line);
                }
                Event::TaskListMarker(done) => {
                    self.push_words(if done { "[x] " } else { "[ ] " });
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
                if level <= HeadingLevel::H2 {
                    self.push_raw_style(GREEN);
                }
                let hashes = "#".repeat(heading_rank(level));
                self.push_words(&format!("{hashes} "));
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
                    self.lines.push(format!("{DIM}```{lang}{RESET}"));
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
                self.push_words(&marker);
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
            TagEnd::Heading(_) => {
                self.style.bold = false;
                self.flush_line();
            }
            TagEnd::BlockQuote(_) => {
                self.quote_depth -= 1;
                self.style.dim = self.quote_depth > 0;
                self.update_indent();
                self.flush_line();
            }
            TagEnd::CodeBlock => {
                self.in_code_block = false;
                if self.code_lang.take().is_some() {
                    self.lines.push(format!("{DIM}```{RESET}"));
                }
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
            prefix = format!("{DIM}{prefix}");
        }
        self.indent = prefix;
    }

    fn push_raw_style(&mut self, style: &str) {
        self.ensure_line_started();
        self.current.push_str(style);
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
        let mut code_lines: Vec<&str> = text.split('\n').collect();
        if text.ends_with('\n') {
            code_lines.pop();
        }
        let syntax = self
            .code_lang
            .as_deref()
            .and_then(|lang| SYNTAXES.find_syntax_by_token(lang));
        match syntax {
            Some(syntax) => {
                let mut highlighter = HighlightLines::new(syntax, &THEME);
                for line in code_lines {
                    let styled = highlighter
                        .highlight_line(line, &SYNTAXES)
                        .map(|ranges| as_24_bit_terminal_escaped(&ranges, false))
                        .unwrap_or_else(|_| line.to_owned());
                    self.lines.push(format!("  {styled}{RESET}"));
                }
            }
            None => {
                for line in code_lines {
                    self.lines.push(format!("  {DIM}{line}{RESET}"));
                }
            }
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

fn heading_rank(level: HeadingLevel) -> usize {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// Split text into words and single-space separators (runs collapse).
fn split_words(text: &str) -> Vec<&str> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::visible_width;

    fn plain(lines: &[String]) -> Vec<String> {
        lines
            .iter()
            .map(|l| crate::terminal::strip_ansi(l))
            .collect()
    }

    #[test]
    fn renders_paragraphs_with_wrapping() {
        let lines = render_markdown("one two three four five six seven", 20);
        assert!(lines.len() > 1);
        for line in &lines {
            assert!(visible_width(line) <= 20, "too wide: {line:?}");
        }
        assert_eq!(plain(&lines).join(" "), "one two three four five six seven");
    }

    #[test]
    fn renders_heading_bold() {
        let lines = render_markdown("# Title\n\nbody", 80);
        assert!(lines[0].contains(BOLD));
        assert!(plain(&lines)[0].contains("# Title"));
        assert_eq!(plain(&lines).last().unwrap(), "body");
    }

    #[test]
    fn renders_code_block_verbatim() {
        let lines = render_markdown("```rust\nlet x = 1;\nlet y = 2;\n```", 80);
        let p = plain(&lines);
        assert!(p.iter().any(|l| l.contains("let x = 1;")));
        assert!(p.iter().any(|l| l.contains("let y = 2;")));
    }

    #[test]
    fn renders_lists_with_bullets_and_numbers() {
        let p = plain(&render_markdown("- a\n- b\n\n1. x\n2. y", 80));
        assert!(p.iter().any(|l| l.trim_start().starts_with("• a")), "{p:?}");
        assert!(
            p.iter().any(|l| l.trim_start().starts_with("1. x")),
            "{p:?}"
        );
        assert!(
            p.iter().any(|l| l.trim_start().starts_with("2. y")),
            "{p:?}"
        );
    }

    #[test]
    fn wrapped_list_items_hang_under_text() {
        let p = plain(&render_markdown(
            "- alpha beta gamma delta epsilon\n- short",
            20,
        ));
        assert!(p[0].starts_with("• alpha"), "{p:?}");
        // Continuations align under the item text, past the marker.
        assert!(p[1].starts_with("  ") && !p[1].starts_with("   "), "{p:?}");
        assert!(p.iter().any(|l| l.starts_with("• short")), "{p:?}");

        let p = plain(&render_markdown("1. alpha beta gamma delta epsilon", 20));
        assert!(p[0].starts_with("1. alpha"), "{p:?}");
        assert!(
            p[1].starts_with("   ") && !p[1].starts_with("    "),
            "{p:?}"
        );
    }

    #[test]
    fn inline_code_styled() {
        let lines = render_markdown("use `foo()` here", 80);
        assert!(lines[0].contains(CYAN));
        assert_eq!(plain(&lines)[0], "use foo() here");
    }

    #[test]
    fn links_show_styled_text_and_url() {
        let lines = render_markdown("see [docs](https://example.com) now", 80);
        assert!(lines[0].contains(UNDERLINE));
        assert_eq!(plain(&lines)[0], "see docs (https://example.com) now");
        // Autolinks don't duplicate the URL.
        let lines = render_markdown("go to <https://example.com>", 80);
        assert_eq!(plain(&lines)[0], "go to https://example.com");
    }

    #[test]
    fn fenced_code_is_highlighted_and_closed() {
        let lines = render_markdown("```rust\nlet x = 1;\n```", 80);
        let p = plain(&lines);
        assert_eq!(p.first().unwrap(), "```rust");
        assert_eq!(p.last().unwrap(), "```");
        // Highlighted lines carry truecolor escapes, not plain DIM.
        let code = lines
            .iter()
            .find(|l| crate::terminal::strip_ansi(l).contains("let x"))
            .unwrap();
        assert!(code.contains("\x1b[38;2;"), "no highlight: {code:?}");
    }

    #[test]
    fn unknown_language_falls_back_to_dim() {
        let lines = render_markdown("```nosuchlang\nfoo bar\n```", 80);
        let code = lines.iter().find(|l| l.contains("foo bar")).unwrap();
        assert!(code.contains(DIM));
    }

    #[test]
    fn blockquote_prefixed() {
        let p = plain(&render_markdown("> quoted text", 80));
        assert!(p.iter().any(|l| l.contains("│ quoted text")), "{p:?}");
    }
}
