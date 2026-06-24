use super::constants::{MarkdownPalette, RESET, UNDERLINE};

/// stderr, logs). Base text renders as a quiet gray; error/warning/success
/// keywords, file paths, numbers/durations, quoted spans and URLs get colored
/// accents. Lines that already carry ANSI escapes pass through unchanged so
/// styled output is never double-highlighted. Highlighted lines always end
/// with a reset.
pub fn highlight_output(line: &str, palette: &MarkdownPalette) -> String {
    if line.contains('\x1b') {
        return line.to_owned();
    }
    let mut out = String::with_capacity(line.len() + 32);
    out.push_str(&palette.out_base);
    let mut i = 0;
    while i < line.len() {
        let ch = line[i..].chars().next().unwrap();
        if (ch == '"' || ch == '\'' || ch == '`')
            && let Some(off) = line[i + ch.len_utf8()..].find(ch)
        {
            let end = i + ch.len_utf8() + off + ch.len_utf8();
            out.push_str(&palette.out_str);
            out.push_str(&line[i..end]);
            out.push_str(&palette.out_base);
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
        match classify_output_token(core, palette) {
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
                out.push_str(&palette.out_base);
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
pub(super) fn split_output_token(token: &str) -> (&str, &str, &str) {
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
pub(super) fn classify_output_token<'p>(
    core: &str,
    palette: &'p MarkdownPalette,
) -> Option<(&'p str, bool)> {
    if core.is_empty() {
        return None;
    }
    if core.starts_with("http://") || core.starts_with("https://") {
        return Some((&palette.out_url, true));
    }
    let lower = core.to_ascii_lowercase();
    match lower.as_str() {
        "error" | "errors" | "panicked" | "traceback" | "failed" | "failure" | "exception" => {
            return Some((&palette.out_err, false));
        }
        "warning" | "warnings" | "warn" | "deprecated" => return Some((&palette.out_warn, false)),
        "ok" | "passed" | "success" | "finished" => return Some((&palette.out_ok, false)),
        _ => {}
    }
    if lower.starts_with("error[") {
        return Some((&palette.out_err, false));
    }
    if looks_like_path(core) {
        return Some((&palette.out_path, false));
    }
    if looks_like_number(core) {
        return Some((&palette.out_num, false));
    }
    None
}

pub(super) fn looks_like_path(tok: &str) -> bool {
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

pub(super) fn looks_like_number(tok: &str) -> bool {
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
