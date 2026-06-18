use pulldown_cmark::CodeBlockKind;
use syntect::easy::HighlightLines;
use syntect::util::as_24_bit_terminal_escaped;

use super::constants::*;

pub(super) fn normalize_code_lang(lang: &str) -> String {
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

pub(super) fn highlight_code_lines(code_lines: Vec<&str>, lang: Option<&str>) -> Vec<String> {
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

pub(super) fn highlight_repl_lines(code_lines: Vec<&str>) -> Vec<String> {
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
pub(super) fn soften_ansi(text: &str) -> String {
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

pub(super) fn soften_sgr_params(params: &str) -> String {
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
pub(super) fn soften_rgb(r: u8, g: u8, b: u8) -> (u8, u8, u8) {
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

pub(super) fn strip_ansi(text: &str) -> String {
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

pub(super) fn next_visible_char(text: &str, i: usize) -> Option<(usize, char)> {
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

pub(super) fn visible_after_call(text: &str, mut i: usize) -> bool {
    while let Some((j, ch)) = next_visible_char(text, i) {
        if ch == ' ' || ch == '\t' {
            i = j + ch.len_utf8();
            continue;
        }
        return ch == '(';
    }
    false
}

pub(super) fn highlight_known_repl_calls(line: impl AsRef<str>) -> String {
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
