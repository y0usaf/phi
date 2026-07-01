use std::collections::{HashMap, VecDeque, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};
use std::sync::{LazyLock, Mutex};

use syntect::easy::HighlightLines;
use syntect::util::as_24_bit_terminal_escaped;

use super::constants::{MarkdownPalette, SYNTAXES};

/// Maximum number of highlighted code blocks kept in the cache.
const HIGHLIGHT_CACHE_LIMIT: usize = 256;

/// Thread-safe cache for syntax-highlighted code blocks.
/// Keyed by `hash_code(code, lang)`; stores the ANSI-escaped output lines.
/// `order` tracks insertion order so eviction drops only the oldest entry —
/// clearing wholesale would cold-start every visible block at once.
#[derive(Default)]
struct HighlightCache {
    entries: HashMap<u64, Vec<String>>,
    order: VecDeque<u64>,
}

static HIGHLIGHT_CACHE: LazyLock<Mutex<HighlightCache>> =
    LazyLock::new(|| Mutex::new(HighlightCache::default()));

/// Hash a code block's content and language tag to a 64-bit key.
fn hash_code(code: &str, lang: Option<&str>) -> u64 {
    let mut hasher = DefaultHasher::new();
    code.hash(&mut hasher);
    lang.hash(&mut hasher);
    hasher.finish()
}

/// Highlight a code block using a thread-safe cache.
///
/// On a cache hit the previously rendered ANSI lines are returned without
/// re-tokenizing. On a miss the code is highlighted via `highlight_code`
/// and stored for subsequent calls. Past `HIGHLIGHT_CACHE_LIMIT` entries,
/// the oldest-inserted entries are evicted one at a time.
#[allow(clippy::collapsible_if)]
pub fn highlight_code_cached(
    code: &str,
    lang: Option<&str>,
    palette: &MarkdownPalette,
) -> Vec<String> {
    let hash = hash_code(code, lang);

    if let Ok(cache) = HIGHLIGHT_CACHE.lock() {
        if let Some(lines) = cache.entries.get(&hash).cloned() {
            return lines;
        }
    }

    let lines = highlight_code(code, lang, palette);

    if let Ok(mut cache) = HIGHLIGHT_CACHE.lock() {
        // A racing miss for the same block may have inserted already; the
        // key goes into `order` only when it is genuinely new so the two
        // structures stay 1:1.
        if cache.entries.insert(hash, lines.clone()).is_none() {
            cache.order.push_back(hash);
        }
        while cache.entries.len() > HIGHLIGHT_CACHE_LIMIT {
            let Some(oldest) = cache.order.pop_front() else {
                break;
            };
            cache.entries.remove(&oldest);
        }
    }

    lines
}

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
pub fn highlight_code(text: &str, lang: Option<&str>, palette: &MarkdownPalette) -> Vec<String> {
    let mut code_lines: Vec<&str> = text.split('\n').collect();
    if text.ends_with('\n') {
        code_lines.pop();
    }
    highlight_code_lines(code_lines, lang, palette)
}

pub(super) fn highlight_code_lines(
    code_lines: Vec<&str>,
    lang: Option<&str>,
    palette: &MarkdownPalette,
) -> Vec<String> {
    if matches!(lang, Some(l) if l.trim().eq_ignore_ascii_case("repl")) {
        return highlight_repl_lines(code_lines, palette);
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
            let mut highlighter = HighlightLines::new(syntax, &palette.syntax_theme);
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

pub(super) fn highlight_repl_lines(
    code_lines: Vec<&str>,
    palette: &MarkdownPalette,
) -> Vec<String> {
    let syntax = SYNTAXES.find_syntax_by_token("python");
    match syntax {
        Some(syntax) => {
            let mut highlighter = HighlightLines::new(syntax, &palette.syntax_theme);
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
            // syntect always emits truecolor; this pass is also where the
            // output gets quantized for terminals that can't render it.
            if crate::color_cap::has_truecolor() {
                out.push("38".to_owned());
                out.push("2".to_owned());
                out.push(r.to_string());
                out.push(g.to_string());
                out.push(b.to_string());
            } else {
                out.push("38".to_owned());
                out.push("5".to_owned());
                out.push(crate::color_cap::rgb_to_xterm256(r, g, b).to_string());
            }
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
