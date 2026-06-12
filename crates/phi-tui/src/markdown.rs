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
const REPL_FENCE: &str = "\x1b[38;2;180;140;255m";
const REPL_FN: &str = "\x1b[38;2;255;210;120m";

static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
static THEME: LazyLock<Theme> = LazyLock::new(|| {
    ThemeSet::load_defaults()
        .themes
        .remove("base16-ocean.dark")
        .expect("bundled theme")
});

fn normalize_code_lang(lang: &str) -> String {
    match lang.trim().to_ascii_lowercase().as_str() {
        "rs" => "rust".to_owned(),
        "js" | "javascript" | "jsx" | "ts" | "typescript" | "tsx" => "javascript".to_owned(),
        "py" | "repl" => "python".to_owned(),
        "sh" | "shell" | "zsh" | "fish" | "console" | "terminal" => "bash".to_owned(),
        "yml" => "yaml".to_owned(),
        "md" => "markdown".to_owned(),
        "dockerfile" => "docker".to_owned(),
        other => other.to_owned(),
    }
}

/// Highlight source code as ANSI-styled lines using the bundled syntect theme.
/// Common aliases are normalized (notably `repl` -> `python`).
pub fn highlight_code(text: &str, lang: Option<&str>) -> Vec<String> {
    let mut code_lines: Vec<&str> = text.split('\n').collect();
    if text.ends_with('\n') {
        code_lines.pop();
    }
    if matches!(lang, Some(l) if l.trim().eq_ignore_ascii_case("repl")) {
        return highlight_repl_lines(code_lines);
    }
    let syntax = lang
        .map(normalize_code_lang)
        .and_then(|lang| SYNTAXES.find_syntax_by_token(&lang));
    match syntax {
        Some(syntax) => {
            let mut highlighter = HighlightLines::new(syntax, &THEME);
            code_lines
                .into_iter()
                .map(|line| {
                    highlighter
                        .highlight_line(line, &SYNTAXES)
                        .map(|ranges| as_24_bit_terminal_escaped(&ranges, false))
                        .unwrap_or_else(|_| line.to_owned())
                })
                .collect()
        }
        None => code_lines.into_iter().map(str::to_owned).collect(),
    }
}

fn highlight_repl_lines(code_lines: Vec<&str>) -> Vec<String> {
    let syntax = SYNTAXES.find_syntax_by_token("python");
    match syntax {
        Some(syntax) => {
            let mut highlighter = HighlightLines::new(syntax, &THEME);
            code_lines
                .into_iter()
                .map(|line| {
                    let base = highlighter
                        .highlight_line(line, &SYNTAXES)
                        .map(|ranges| as_24_bit_terminal_escaped(&ranges, false))
                        .unwrap_or_else(|_| line.to_owned());
                    highlight_known_repl_calls(&base)
                })
                .collect()
        }
        None => code_lines
            .into_iter()
            .map(highlight_known_repl_calls)
            .collect(),
    }
}

fn strip_ansi(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            i += 1;
            if i < bytes.len() && bytes[i] == b'[' {
                i += 1;
                while i < bytes.len() && bytes[i] != b'm' {
                    i += 1;
                }
                if i < bytes.len() {
                    i += 1;
                }
            }
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

fn next_visible_char(text: &str, i: usize) -> Option<(usize, char)> {
    let bytes = text.as_bytes();
    let mut j = i;
    while j < bytes.len() {
        if bytes[j] == 0x1b {
            j += 1;
            if j < bytes.len() && bytes[j] == b'[' {
                j += 1;
                while j < bytes.len() && bytes[j] != b'm' {
                    j += 1;
                }
                if j < bytes.len() {
                    j += 1;
                }
            }
            continue;
        }
        let ch = text[j..].chars().next()?;
        return Some((j, ch));
    }
    None
}

fn visible_after_call(text: &str, mut i: usize) -> bool {
    while let Some((j, ch)) = next_visible_char(text, i) {
        if ch == ' ' || ch == '\t' {
            i = j + ch.len_utf8();
            continue;
        }
        return ch == '(';
    }
    false
}

fn highlight_known_repl_calls(line: impl AsRef<str>) -> String {
    const NAMES: &[&str] = &[
        "bash",
        "read_file",
        "write_file",
        "edit_file",
        "grep",
        "find",
        "ls",
        "llm_query",
        "llm_query_batched",
        "rlm_query",
        "rlm_query_batched",
        "SHOW_VARS",
        "jobs",
        "job_status",
        "job_output",
        "bg",
    ];
    let line = line.as_ref();
    let plain = strip_ansi(line);
    let mut hits: Vec<String> = NAMES
        .iter()
        .copied()
        .filter(|name| {
            let pat = format!("{name}(");
            plain.contains(&pat) || plain.contains(&format!("{name} ("))
        })
        .map(str::to_owned)
        .collect();
    hits.sort_by_key(|s| std::cmp::Reverse(s.len()));

    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len() + 64);
    let mut i = 0;
    let mut prev_visible_ident = false;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            let start = i;
            i += 1;
            if i < bytes.len() && bytes[i] == b'[' {
                i += 1;
                while i < bytes.len() && bytes[i] != b'm' {
                    i += 1;
                }
                if i < bytes.len() {
                    i += 1;
                }
            }
            out.push_str(&line[start..i]);
            continue;
        }

        let mut matched = None;
        for name in &hits {
            if !line[i..].starts_with(name) {
                continue;
            }
            if prev_visible_ident {
                continue;
            }
            if visible_after_call(line, i + name.len()) {
                matched = Some(name.as_str());
                break;
            }
        }

        if let Some(name) = matched {
            out.push_str(REPL_FN);
            out.push_str(name);
            out.push_str(RESET);
            i += name.len();
            prev_visible_ident = true;
            continue;
        }

        let ch = line[i..].chars().next().unwrap();
        out.push(ch);
        prev_visible_ident = ch.is_ascii_alphanumeric() || ch == '_';
        i += ch.len_utf8();
    }
    out
}

#[derive(Clone, Copy, Default)]
struct Style {
    bold: bool,
    italic: bool,
    code: bool,
    dim: bool,
    link: bool,
    heading: bool,
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
                    let line = format!("{DIM}{}{RESET}", "─".repeat(self.width));
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
                self.style.heading = level <= HeadingLevel::H2;
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
            TagEnd::Heading(level) => {
                self.style.bold = false;
                self.style.heading = false;
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
            prefix = format!("{DIM}{prefix}");
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
        assert!(lines[0].contains(CYAN));
        let p = plain(&lines);
        assert_eq!(p[0], "Title");
        assert!(!p[0].contains('#'), "literal markdown marker leaked: {p:?}");
        // H1 carries an underline rule sized to the title.
        assert_eq!(p[1], "─".repeat(5));
        assert_eq!(p.last().unwrap(), "body");
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
        assert_eq!(p.first().unwrap(), "\u{258d} rust");
        assert!(p.last().unwrap().starts_with('\u{258f}'), "{p:?}");
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
    fn common_language_aliases_are_highlighted() {
        for lang in ["ts", "tsx", "js", "jsx", "py", "sh", "shell", "console"] {
            let sample = match lang {
                "py" => "print(1)",
                "sh" | "shell" | "console" => "echo hi",
                "ts" | "tsx" => "const x: number = 1;",
                _ => "const x = 1;",
            };
            let lines = render_markdown(&format!("```{lang}\n{sample}\n```"), 80);
            let code = lines
                .iter()
                .find(|l| crate::terminal::strip_ansi(l).contains(sample))
                .unwrap();
            assert!(
                code.contains("\x1b[38;2;"),
                "alias {lang} was not highlighted: {code:?}"
            );
        }
    }

    #[test]
    fn blockquote_prefixed() {
        let p = plain(&render_markdown("> quoted text", 80));
        assert!(p.iter().any(|l| l.contains("│ quoted text")), "{p:?}");
    }

    #[test]
    fn highlight_code_treats_repl_as_python() {
        let lines = highlight_code("print(1)", Some("repl"));
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("\x1b[38;2;"), "no highlight: {lines:?}");
    }

    #[test]
    fn repl_highlighting_marks_known_calls() {
        let lines = highlight_code("print(bash('echo hi'))", Some("repl"));
        assert_eq!(lines.len(), 1);
        let stripped = strip_ansi(&lines[0]);
        assert!(stripped.contains("bash"), "known repl call missing: {lines:?}");
        assert!(lines[0].contains(REPL_FN), "known repl call not highlighted: {lines:?}");
    }

    #[test]
    fn markdown_renders_repl_fences_literally() {
        let lines = render_markdown("```repl\nprint(1)\n```", 80);
        let p = plain(&lines);
        assert_eq!(p.first().unwrap(), "```repl");
        assert_eq!(p.last().unwrap(), "```");
    }
}
