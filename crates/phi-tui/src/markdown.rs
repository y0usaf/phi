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
const MARKER_ACCENT: &str = "\x1b[38;2;130;160;230m";
const TASK_DONE: &str = "\x1b[38;2;130;200;130m";
const QUOTE_BAR: &str = "\x1b[38;2;150;130;220m";
const SUBHEAD: &str = "\x1b[38;2;190;150;255m";
const OUT_BASE: &str = "\x1b[38;2;150;155;165m";
const OUT_ERR: &str = "\x1b[38;2;240;120;120m";
const OUT_WARN: &str = "\x1b[38;2;235;185;100m";
const OUT_OK: &str = "\x1b[38;2;130;200;140m";
const OUT_PATH: &str = "\x1b[38;2;130;180;230m";
const OUT_NUM: &str = "\x1b[38;2;170;150;230m";
const OUT_STR: &str = "\x1b[38;2;150;190;150m";
const OUT_URL: &str = "\x1b[38;2;120;200;220m";
const DIFF_ADD: &str = "\x1b[38;2;120;200;130m";
const DIFF_DEL: &str = "\x1b[38;2;230;110;110m";

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

/// Highlight source code as ANSI-styled lines using the bundled syntect
/// theme. Common aliases are normalized (notably `repl` -> `python`).
/// Near-white default tokens are softened to gray: in this UI, white is
/// reserved for model prose, so code must never render as plain white.
pub fn highlight_code(text: &str, lang: Option<&str>) -> Vec<String> {
    let mut code_lines: Vec<&str> = text.split('\n').collect();
    if text.ends_with('\n') {
        code_lines.pop();
    }
    highlight_code_lines(code_lines, lang)
}

/// Highlight a snippet when a language hint is available, otherwise return
/// plain lines unchanged. Useful for previews nested inside the REPL UI.
pub fn highlight_snippet(text: &str, lang: Option<&str>) -> Vec<String> {
    highlight_code(text, lang)
}

fn highlight_code_lines(code_lines: Vec<&str>, lang: Option<&str>) -> Vec<String> {
    if matches!(lang, Some(l) if l.trim().eq_ignore_ascii_case("repl")) {
        return highlight_repl_lines(code_lines);
    }
    let syntax = lang.map(normalize_code_lang).and_then(|lang| {
        SYNTAXES.find_syntax_by_token(&lang).or_else(|| {
            code_lines
                .first()
                .and_then(|first| SYNTAXES.find_syntax_by_first_line(first))
        })
    });
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
                    soften_ansi(&base)
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
                    let softened = soften_ansi(&base);
                    highlight_known_repl_calls(&softened)
                })
                .collect()
        }
        None => code_lines
            .into_iter()
            .map(highlight_known_repl_calls)
            .collect(),
    }
}

/// Post-pass over syntect output: rewrite near-white 24-bit foregrounds to a
/// quiet gray. Colored tokens (keywords, strings, calls) pass through, so
/// syntax still carries the structure -- but the body of code reads a step
/// below model prose, which is the only white text on screen.
fn soften_ansi(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            let start = i;
            i += 1;
            if i < bytes.len() && bytes[i] == b'[' {
                i += 1;
                while i < bytes.len() && bytes[i] != b'm' {
                    i += 1;
                }
                if i < bytes.len() && bytes[i] == b'm' {
                    let params = &text[start + 2..i];
                    i += 1;
                    out.push_str("\x1b[");
                    out.push_str(&soften_sgr_params(params));
                    out.push('m');
                    continue;
                }
            }
            out.push_str(&text[start..i]);
            continue;
        }

        let ch = text[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn soften_sgr_params(params: &str) -> String {
    let parts: Vec<&str> = if params.is_empty() {
        Vec::new()
    } else {
        params.split(';').collect()
    };
    let mut out: Vec<String> = Vec::with_capacity(parts.len());
    let mut i = 0;
    while i < parts.len() {
        if parts[i] == "38"
            && parts.get(i + 1) == Some(&"2")
            && i + 4 < parts.len()
            && let (Ok(r), Ok(g), Ok(b)) = (
                parts[i + 2].parse::<u8>(),
                parts[i + 3].parse::<u8>(),
                parts[i + 4].parse::<u8>(),
            )
        {
            let (r, g, b) = soften_rgb(r, g, b);
            out.push("38".to_owned());
            out.push("2".to_owned());
            out.push(r.to_string());
            out.push(g.to_string());
            out.push(b.to_string());
            i += 5;
            continue;
        }
        out.push(parts[i].to_owned());
        i += 1;
    }
    out.join(";")
}

/// Near-white (all channels bright) becomes gray; near-gray passes through;
/// everything else gets a saturation boost (~35%) so the muted base16-ocean
/// palette reads vivid. Boosted colors are clamped and can never become
/// near-white (min channel >= 165 is never produced by a boost).
fn soften_rgb(r: u8, g: u8, b: u8) -> (u8, u8, u8) {
    let min = r.min(g).min(b);
    let max = r.max(g).max(b);
    if min >= 165 {
        let scale = |v: u8| ((v as u16 * 78) / 100) as u8;
        return (scale(r), scale(g), scale(b));
    }
    // Near-gray: too little chroma for a boost to mean anything.
    if max - min < 24 {
        return (r, g, b);
    }
    let mean = (r as i32 + g as i32 + b as i32) / 3;
    let boost = |v: u8| (mean + (v as i32 - mean) * 135 / 100).clamp(0, 255) as u8;
    let (br, bg, bb) = (boost(r), boost(g), boost(b));
    if br.min(bg).min(bb) >= 165 {
        return (r, g, b);
    }
    (br, bg, bb)
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

/// Micro-grammar highlighter for tool/REPL execution output lines (stdout,
/// stderr, logs). Base text renders as a quiet gray; error/warning/success
/// keywords, file paths, numbers/durations, quoted spans and URLs get colored
/// accents. Lines that already carry ANSI escapes pass through unchanged so
/// styled output is never double-highlighted. Highlighted lines always end
/// with a reset.
pub fn highlight_output(line: &str) -> String {
    if line.contains('\x1b') {
        return line.to_owned();
    }
    let mut out = String::with_capacity(line.len() + 32);
    out.push_str(OUT_BASE);
    let mut i = 0;
    while i < line.len() {
        let ch = line[i..].chars().next().unwrap();
        if (ch == '"' || ch == '\'' || ch == '`')
            && let Some(off) = line[i + ch.len_utf8()..].find(ch)
        {
            let end = i + ch.len_utf8() + off + ch.len_utf8();
            out.push_str(OUT_STR);
            out.push_str(&line[i..end]);
            out.push_str(OUT_BASE);
            i = end;
            continue;
        }
        if ch.is_whitespace() {
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        let end = line[i..]
            .find(char::is_whitespace)
            .map(|o| i + o)
            .unwrap_or(line.len());
        let token = &line[i..end];
        let (lead, core, trail) = split_output_token(token);
        match classify_output_token(core) {
            Some((color, underline)) => {
                out.push_str(lead);
                if underline {
                    out.push_str(UNDERLINE);
                }
                out.push_str(color);
                out.push_str(core);
                if underline {
                    out.push_str("\x1b[24m");
                }
                out.push_str(OUT_BASE);
                out.push_str(trail);
            }
            None => out.push_str(token),
        }
        i = end;
    }
    out.push_str(RESET);
    out
}

/// Split a whitespace-delimited token into leading brackets, a core, and
/// trailing punctuation, so `(error)` or `src/foo.rs:12:5,` classify cleanly.
fn split_output_token(token: &str) -> (&str, &str, &str) {
    let start = token
        .find(|c: char| !matches!(c, '(' | '[' | '{' | '<'))
        .unwrap_or(token.len());
    let (lead, rest) = token.split_at(start);
    let end = rest
        .rfind(|c: char| !matches!(c, ')' | ']' | '}' | '>' | ',' | ';' | ':' | '!' | '?' | '.'))
        .map(|p| p + rest[p..].chars().next().unwrap().len_utf8())
        .unwrap_or(0);
    let (core, trail) = rest.split_at(end);
    (lead, core, trail)
}

/// Returns the accent color (and whether to underline) for an output token.
fn classify_output_token(core: &str) -> Option<(&'static str, bool)> {
    if core.is_empty() {
        return None;
    }
    if core.starts_with("http://") || core.starts_with("https://") {
        return Some((OUT_URL, true));
    }
    let lower = core.to_ascii_lowercase();
    match lower.as_str() {
        "error" | "errors" | "panicked" | "traceback" | "failed" | "failure" | "exception" => {
            return Some((OUT_ERR, false));
        }
        "warning" | "warnings" | "warn" | "deprecated" => return Some((OUT_WARN, false)),
        "ok" | "passed" | "success" | "finished" => return Some((OUT_OK, false)),
        _ => {}
    }
    if lower.starts_with("error[") {
        return Some((OUT_ERR, false));
    }
    if looks_like_path(core) {
        return Some((OUT_PATH, false));
    }
    if looks_like_number(core) {
        return Some((OUT_NUM, false));
    }
    None
}

fn looks_like_path(tok: &str) -> bool {
    if tok.contains("://") {
        return false;
    }
    if let Some(idx) = tok.rfind('/') {
        let tail = &tok[idx + 1..];
        return !tail.is_empty()
            && (tail.contains('.')
                || tok.starts_with('/')
                || tok.starts_with("./")
                || tok.starts_with("../"));
    }
    // bare `file.ext:line[:col]`
    let mut parts = tok.split(':');
    let head = parts.next().unwrap_or("");
    let rest: Vec<&str> = parts.collect();
    !rest.is_empty()
        && head.contains('.')
        && !head.starts_with('.')
        && rest
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

fn looks_like_number(tok: &str) -> bool {
    let core = tok
        .strip_suffix("ms")
        .or_else(|| tok.strip_suffix("us"))
        .or_else(|| tok.strip_suffix("ns"))
        .or_else(|| tok.strip_suffix('s'))
        .or_else(|| tok.strip_suffix('m'))
        .or_else(|| tok.strip_suffix('h'))
        .or_else(|| tok.strip_suffix('%'))
        .or_else(|| tok.strip_suffix('x'))
        .unwrap_or(tok);
    if core.is_empty() || !core.bytes().any(|b| b.is_ascii_digit()) {
        return false;
    }
    let mut dots = 0;
    core.chars().all(|c| {
        if c == '.' {
            dots += 1;
            dots <= 1
        } else {
            c.is_ascii_digit()
        }
    })
}

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
struct Style {
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
        assert!(
            stripped.contains("bash"),
            "known repl call missing: {lines:?}"
        );
        assert!(
            lines[0].contains(REPL_FN),
            "known repl call not highlighted: {lines:?}"
        );
    }

    #[test]
    fn highlight_snippet_plain_without_lang() {
        let lines = highlight_snippet("plain text", None);
        assert_eq!(lines, vec!["plain text"]);
    }

    #[test]
    fn highlight_snippet_with_lang_uses_syntax_theme() {
        let lines = highlight_snippet("fn main() {}", Some("rust"));
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("\x1b[38;2;"), "no highlight: {lines:?}");
    }

    #[test]
    fn code_default_tokens_never_render_white() {
        // base16-ocean.dark default fg is #c0c5ce (192;197;206): near-white.
        // It must be softened to gray so only model prose reads as white.
        let lines = highlight_code("fn main() {}", Some("rust"));
        assert_eq!(lines.len(), 1);
        assert!(
            !lines[0].contains("38;2;192;197;206"),
            "near-white token survived softening: {lines:?}"
        );
        assert!(lines[0].contains("\x1b[38;2;"), "no highlight: {lines:?}");
    }

    #[test]
    fn output_error_keyword_colored() {
        let line = highlight_output("compile error: something broke");
        assert!(line.starts_with(OUT_BASE), "{line:?}");
        assert!(line.contains(OUT_ERR), "error accent missing: {line:?}");
        assert!(line.ends_with(RESET), "{line:?}");
        assert_eq!(strip_ansi(&line), "compile error: something broke");
    }

    #[test]
    fn output_warning_and_success_colored() {
        let line = highlight_output("warning: deprecated API");
        assert!(line.contains(OUT_WARN), "{line:?}");
        let line = highlight_output("test result: ok. 12 passed");
        assert!(line.contains(OUT_OK), "{line:?}");
    }

    #[test]
    fn output_path_colored() {
        let line = highlight_output("  --> src/foo.rs:12:5");
        assert!(line.contains(OUT_PATH), "path accent missing: {line:?}");
        let line = highlight_output("see /tmp/log.txt for details");
        assert!(line.contains(OUT_PATH), "{line:?}");
    }

    #[test]
    fn output_numbers_strings_urls_colored() {
        let line = highlight_output("done in 1.23s (45ms, 100%)");
        assert!(line.contains(OUT_NUM), "{line:?}");
        let line = highlight_output("read 'config.toml' now");
        assert!(line.contains(OUT_STR), "{line:?}");
        let line = highlight_output("docs at https://example.com/x");
        assert!(line.contains(OUT_URL), "{line:?}");
        assert!(line.contains(UNDERLINE), "{line:?}");
    }

    #[test]
    fn output_with_ansi_passes_through() {
        let pre = "\x1b[31merror\x1b[0m already styled";
        assert_eq!(highlight_output(pre), pre);
    }

    #[test]
    fn output_handles_weird_unicode() {
        let line = highlight_output("héllo → wörld 日本語 error ✓");
        assert!(line.contains(OUT_ERR), "{line:?}");
        assert_eq!(
            crate::terminal::strip_ansi(&line),
            "héllo → wörld 日本語 error ✓"
        );
    }

    #[test]
    fn diff_lines_colored() {
        assert_eq!(
            highlight_diff_line("+added"),
            Some(format!("{DIFF_ADD}+added{RESET}"))
        );
        assert_eq!(
            highlight_diff_line("-removed"),
            Some(format!("{DIFF_DEL}-removed{RESET}"))
        );
        assert_eq!(
            highlight_diff_line("@@ -1,3 +1,4 @@"),
            Some(format!("{REPL_FENCE}@@ -1,3 +1,4 @@{RESET}"))
        );
        assert_eq!(highlight_diff_line("+++ b/file.rs"), None);
        assert_eq!(highlight_diff_line("--- a/file.rs"), None);
        assert_eq!(highlight_diff_line(" context"), None);
    }

    #[test]
    fn vivify_boosts_muted_colors_and_softens_near_white() {
        // base16-ocean red #bf616a: muted, should gain saturation.
        let (r, g, b) = soften_rgb(191, 97, 106);
        let before = 191 - 97;
        let after = r.max(g).max(b) as i32 - r.min(g).min(b) as i32;
        assert!(after > before, "not more saturated: {r},{g},{b}");
        assert!(r.min(g).min(b) < 165, "boost produced near-white");
        // Near-white still softened to gray.
        let (r, g, b) = soften_rgb(192, 197, 206);
        assert!(
            r.max(g).max(b) < 165,
            "near-white not softened: {r},{g},{b}"
        );
        // Near-gray untouched.
        assert_eq!(soften_rgb(100, 105, 110), (100, 105, 110));
    }

    #[test]
    fn list_markers_get_accent_color() {
        let lines = render_markdown("- alpha\n\n1. beta", 80);
        let bullet = lines.iter().find(|l| l.contains('•')).unwrap();
        assert!(bullet.contains(MARKER_ACCENT), "{bullet:?}");
        let num = lines
            .iter()
            .find(|l| crate::terminal::strip_ansi(l).contains("1. beta"))
            .unwrap();
        assert!(num.contains(MARKER_ACCENT), "{num:?}");
        // Item text itself is not accented: accent appears before marker only.
        let after_marker = &bullet[bullet.find("alpha").unwrap()..];
        assert!(!after_marker.contains(MARKER_ACCENT), "{bullet:?}");
    }

    #[test]
    fn task_list_markers_colored() {
        let lines = render_markdown("- [x] done thing\n- [ ] todo thing", 80);
        let done = lines
            .iter()
            .find(|l| crate::terminal::strip_ansi(l).contains("[x]"))
            .unwrap();
        assert!(done.contains(TASK_DONE), "{done:?}");
        let todo = lines
            .iter()
            .find(|l| crate::terminal::strip_ansi(l).contains("[ ]"))
            .unwrap();
        assert!(todo.contains(DIM), "{todo:?}");
    }

    #[test]
    fn blockquote_bar_uses_accent() {
        let lines = render_markdown("> quoted text", 80);
        let q = lines
            .iter()
            .find(|l| crate::terminal::strip_ansi(l).contains("│ quoted text"))
            .unwrap();
        assert!(q.contains(QUOTE_BAR), "{q:?}");
    }

    #[test]
    fn h3_headings_use_subhead_accent() {
        let lines = render_markdown("### Sub Title", 80);
        assert!(lines[0].contains(BOLD), "{lines:?}");
        assert!(lines[0].contains(SUBHEAD), "{lines:?}");
        let lines = render_markdown("## Big Title", 80);
        assert!(lines[0].contains(CYAN), "{lines:?}");
        assert!(!lines[0].contains(SUBHEAD), "{lines:?}");
    }

    #[test]
    fn markdown_renders_repl_fences_literally() {
        let lines = render_markdown("```repl\nprint(1)\n```", 80);
        let p = plain(&lines);
        assert_eq!(p.first().unwrap(), "```repl");
        assert_eq!(p.last().unwrap(), "```");
    }
}
