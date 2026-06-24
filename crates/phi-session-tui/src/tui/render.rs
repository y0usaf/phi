//! Rendering helpers: text formatting, panel drawing, transcript blocks.

use phi_ai::ThinkingLevel;
use phi_session::session;

use super::*;

pub(crate) fn effort_label(level: ThinkingLevel, width: usize) -> Option<String> {
    if level == ThinkingLevel::Off {
        return None;
    }
    let full = match level {
        ThinkingLevel::Off => "OFF",
        ThinkingLevel::Minimal => "MINIMAL",
        ThinkingLevel::Low => "LOW",
        ThinkingLevel::Medium => "MEDIUM",
        ThinkingLevel::High => "HIGH",
        ThinkingLevel::XHigh => "XHIGH",
    };
    if width >= WIDE_TERMINAL_THRESHOLD {
        Some(full.to_string())
    } else {
        Some(full.chars().take(3).collect())
    }
}

// Transcript boxes keep their color in the chrome (borders/titles) only; the
// body stays on the terminal/default background so content styling remains UI-only.

pub(crate) const CODE_PREVIEW_LINES: usize = 8;
pub(crate) const OUTPUT_PREVIEW_LINES: usize = 5;
/// Max visible lines for a live streaming-prose block (scrolls internally).
pub(crate) const LIVE_PROSE_MAX_HEIGHT: usize = 12;
/// Max visible lines for a live thinking block (scrolls internally).
pub(crate) const LIVE_THINKING_MAX_HEIGHT: usize = 6;

pub(crate) const ASSISTANT_THINKING_DISPLAY_OPEN: &str = "<phi-thinking>\n";
pub(crate) const ASSISTANT_THINKING_DISPLAY_CLOSE: &str = "\n</phi-thinking>\n";

pub(crate) fn assistant_display_text(response: &str, thinking: &str) -> Option<String> {
    let thinking = thinking.trim();
    if thinking.is_empty() {
        None
    } else {
        Some(format!(
            "{ASSISTANT_THINKING_DISPLAY_OPEN}{thinking}{ASSISTANT_THINKING_DISPLAY_CLOSE}{response}"
        ))
    }
}

pub(crate) fn split_assistant_display_text(text: &str) -> (Option<&str>, &str) {
    if let Some(rest) = text.strip_prefix(ASSISTANT_THINKING_DISPLAY_OPEN)
        && let Some((thinking, remainder)) = rest.split_once(ASSISTANT_THINKING_DISPLAY_CLOSE)
    {
        (Some(thinking), remainder)
    } else {
        (None, text)
    }
}

/// Byte offset of the first position in `text` that is, or could still grow
/// into, a ```repl fence. The engine's block regex matches mid-line (the
/// model sometimes glues ```repl to the end of a sentence), so this scans
/// raw byte positions rather than line starts.
pub(crate) fn first_block_start(text: &str) -> usize {
    text.char_indices()
        .filter(|&(_, c)| c == '`')
        .map(|(i, _)| i)
        .find(|&i| could_open_block(&text[i..]))
        .unwrap_or(text.len())
}

/// Byte length of the prefix of `text` that is safe to flush into scrollback
/// while streaming: it ends at a completed blank line, leaves no code fence
/// open, and stops before anything that is or could become a ```repl block
/// (which `CodeExecStart` will consume from the stream).
#[cfg(test)]
pub(crate) fn stable_prefix(text: &str) -> usize {
    let limit = first_block_start(text);
    let mut fence_open = false;
    let mut boundary = 0;
    let mut pos = 0;
    for line in text.split_inclusive('\n') {
        pos += line.len();
        if pos > limit {
            break;
        }
        if line.trim_start().starts_with("```") {
            fence_open = !fence_open;
        } else if !fence_open && line.ends_with('\n') && line.trim().is_empty() {
            boundary = pos;
        }
    }
    boundary
}

/// Body of the first live ```repl block once its header newline has landed.
/// Partial headers still return `None`; a closing fence, if already present,
/// is excluded so the body can be previewed before `CodeExecStart` arrives.
pub(crate) fn live_block_body(text: &str) -> Option<&str> {
    let start = first_block_start(text);
    let tail = &text[start..];
    let rest = tail.strip_prefix("```repl")?;
    let mut header_len = "```repl".len();
    let mut saw_newline = false;
    for (i, c) in rest.char_indices() {
        match c {
            ' ' | '\t' => continue,
            '\n' => {
                header_len += i + 1;
                saw_newline = true;
                break;
            }
            _ => return None,
        }
    }
    if !saw_newline {
        return None;
    }
    let body = &tail[header_len..];
    Some(body.split_once("\n```").map_or(body, |(body, _)| body))
}

/// Cap persisted tool results at roughly the replay preview budget. The full
/// output already lives in the repl stdout payload; storing it twice only
/// bloats the log with bytes nothing ever reads back.
pub(crate) const TOOL_RESULT_LOG_BUDGET: usize = 4096;

pub(crate) fn truncate_for_log(text: &str) -> String {
    if text.len() <= TOOL_RESULT_LOG_BUDGET {
        return text.to_owned();
    }
    let mut end = TOOL_RESULT_LOG_BUDGET;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[truncated {} bytes]", &text[..end], text.len() - end)
}

/// Remove stdout lines that are verbatim echoes of tool-call results already
/// rendered as cards in the same block (e.g. `print(grep(...))` literally
/// writes the grep output into stdout). This is a **data-level** coalescing of
/// the stdout stream — it trims echo lines *before* any rendering — never a
/// layout-level deletion of committed rows (scrollback is immutable; see
/// `replace_block_rows`, which only mutates the live block). Returns the
/// residual stdout; when empty, the caller's `push_preview` naturally skips
/// rendering (no-op on empty), so the duplicate card disappears entirely.
pub(crate) fn strip_tool_echoes(stdout: &str, tools: &[session::ToolCall], exec_depth: usize) -> String {
    let mut result_lines: HashSet<&str> = HashSet::new();
    for t in tools {
        if t.error.is_none() && t.depth == exec_depth {
            for line in t.result.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    result_lines.insert(trimmed);
                }
            }
        }
    }
    stdout
        .lines()
        .filter(|line| !result_lines.contains(line.trim()))
        .collect::<Vec<&str>>()
        .join("\n")
}

#[allow(dead_code)]
pub(crate) fn panel_border(label: &str, label_plain: &str, width: usize, color: &str) -> String {
    let fill = width
        .saturating_sub(phi_tui::display_cell_width(label_plain) + 5)
        .max(1);
    format!("{color}┌─ {label}{color} {}┐{RESET}", "─".repeat(fill))
}

#[allow(dead_code)]
pub(crate) fn panel_row(text: &str, width: usize, border: &str) -> String {
    let inner = width.saturating_sub(2).max(1);
    let body = format!(" {text}");
    let pad = inner.saturating_sub(visible_width(&body));
    format!("{border}│{RESET}{body}{RESET}{:pad$}{border}│{RESET}", "")
}

#[allow(dead_code)]
pub(crate) fn panel_bottom(width: usize, color: &str) -> String {
    format!(
        "{color}└{}┘{RESET}",
        "─".repeat(width.saturating_sub(2).max(1))
    )
}

#[allow(dead_code)]
pub(crate) fn fitted_panel_width(label_plain: &str, max_width: usize, body: &[String]) -> usize {
    let label_width = phi_tui::display_cell_width(label_plain).saturating_add(6);
    let body_width = body
        .iter()
        .map(|line| visible_width(line).saturating_add(4))
        .max()
        .unwrap_or(4);
    label_width.max(body_width).min(max_width.max(1)).max(4)
}

/// Per-level nesting indent: two spaces per `depth`. Used to inset a flat
/// header line under its enclosing unit (a repl block or an RLM subcall).
/// Nesting is shown by indentation alone — no glyph marker. Two cells per
/// level keeps `card_body_width(depth)` arithmetic stable across depths.
pub(crate) fn indent(depth: usize) -> String {
    " ".repeat(depth.saturating_mul(2))
}

/// Visible width of `indent(depth)`: `depth * 2` cells. (Equals the old
/// `gutter_width`/`nest_width`, so depth math is unchanged.)
pub(crate) fn indent_width(depth: usize) -> usize {
    depth.saturating_mul(2)
}

/// Result-line gutter: `indent(depth)` plus two spaces. No glyph — the body
/// simply indents under its header. Body content is wrapped to fit after it.
pub(crate) fn result_prefix(depth: usize) -> String {
    format!("{}  ", indent(depth))
}

/// Width of `result_prefix(depth)`'s visible cells: `indent_width(depth) + 2`.
pub(crate) fn result_prefix_width(depth: usize) -> usize {
    indent_width(depth) + 2
}

/// Local wall-clock `HH:MM:SS`, for the `[time]` segment of live-committed
/// `forge_title` headers. Replay/restored rows pass `None` instead (see
/// `forge_title`) so re-rendered transcripts don't stamp a false replay-time
/// clock. Uses `jiff` (already a workspace dep) — not `chrono`.
pub(crate) fn now_hms() -> String {
    jiff::Zoned::now().strftime("%H:%M:%S").to_string()
}

/// Flat header line: `{indent(depth)}[{timestamp}] {title}`. The header is a
/// bare title at the nesting indent (no bullet glyph, no trailing rule).
/// `timestamp == None` omits the `[time]` segment (replay/restored rows).
pub(crate) fn forge_title(depth: usize, timestamp: Option<&str>, title: &str) -> String {
    let stamp = match timestamp {
        Some(t) => format!("{DIM}[{t}] {RESET}"),
        None => String::new(),
    };
    format!("{}{stamp}{title}", indent(depth))
}

/// Flat header line + indented result body. `body` lines must already be
/// wrapped to `card_body_width(depth)`. Every body row is inset by the
/// 2-space result gutter (`result_prefix`). Empty body lines collapse to the
/// bare indent. If `body` is empty the header stands alone.
pub(crate) fn render_flat(
    timestamp: Option<&str>,
    title: &str,
    depth: usize,
    body: &[String],
) -> Vec<String> {
    let header = forge_title(depth, timestamp, title);
    let blank = indent(depth);
    let prefix = result_prefix(depth);
    let mut out = Vec::with_capacity(body.len().saturating_add(1));
    out.push(header);
    for line in body {
        if line.is_empty() {
            out.push(blank.clone());
        } else {
            out.push(format!("{prefix}{line}"));
        }
    }
    out
}

/// REPL block: its activity rows inset by one nesting level. No block header —
/// the code and its output speak for themselves. Body rows are child headers
/// (tool/subcall lines and their own results), so they get a plain 2-space
/// indent on top of their own nesting.
pub(crate) fn render_repl_block(body: &[String], _turn: Option<usize>) -> Vec<String> {
    let prefix = indent(1);
    body.iter()
        .map(|line| {
            if line.is_empty() {
                prefix.clone()
            } else {
                format!("{prefix}{line}")
            }
        })
        .collect()
}

#[derive(Clone)]
pub(crate) struct OpenToolRow {
    pub(crate) depth: usize,
    pub(crate) label: String,
    pub(crate) start: usize,
    pub(crate) len: usize,
}

#[derive(Clone)]
pub(crate) struct OpenSubcallRow {
    pub(crate) depth: usize,
    pub(crate) prompt_preview: String,
    pub(crate) label: String,
    pub(crate) start: usize,
    pub(crate) len: usize,
}

#[derive(Clone)]
pub(crate) struct TranscriptBlock {
    pub(crate) key: String,
    pub(crate) lines: Vec<String>,
    pub(crate) finalized: bool,
    pub(crate) max_height: Option<usize>,
}
