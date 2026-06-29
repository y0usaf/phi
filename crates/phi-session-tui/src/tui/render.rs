//! Rendering helpers: text formatting, panel drawing, transcript blocks.

use phi_ai::ThinkingLevel;
use phi_session::session;

use super::*;

pub(crate) fn effort_label(level: ThinkingLevel, width: usize) -> Option<String> {
    if level == ThinkingLevel::Off {
        return None;
    }
    let full = match level {
        ThinkingLevel::Off => "off",
        ThinkingLevel::Minimal => "minimal",
        ThinkingLevel::Low => "low",
        ThinkingLevel::Medium => "medium",
        ThinkingLevel::High => "high",
        ThinkingLevel::XHigh => "xhigh",
    };
    if width >= WIDE_TERMINAL_THRESHOLD {
        Some(full.to_string())
    } else {
        Some(full.chars().take(3).collect())
    }
}

/// Thinking-level color, mirroring pi's progression: off=gray, minimal=light
/// gray, low=blue, medium=cyan, high=magenta, xhigh=bright magenta.
pub(crate) fn thinking_level_color(level: ThinkingLevel, palette: &super::InlinePalette) -> &str {
    match level {
        ThinkingLevel::Off => &palette.thinking_off,
        ThinkingLevel::Minimal => &palette.thinking_minimal,
        ThinkingLevel::Low => &palette.thinking_low,
        ThinkingLevel::Medium => &palette.thinking_medium,
        ThinkingLevel::High => &palette.thinking_high,
        ThinkingLevel::XHigh => &palette.thinking_xhigh,
    }
}

// Transcript boxes keep their color in the chrome (borders/titles) only; the
// body stays on the terminal/default background so content styling remains UI-only.

pub(crate) const CODE_PREVIEW_LINES: usize = 8;
pub(crate) const OUTPUT_PREVIEW_LINES: usize = 5;

pub(crate) const ASSISTANT_THINKING_DISPLAY_OPEN: &str = "<phi-thinking>\n";
pub(crate) const ASSISTANT_THINKING_DISPLAY_CLOSE: &str = "\n</phi-thinking>\n";

/// OSC 133 zone markers, matching pi's UserMessageComponent / AssistantMessageComponent.
/// A marks the start of a prompt input, B the end of the input, C the end of
/// the model's reply. Shells with OSC 133 support (e.g. fish, zsh) use these
/// to delineate commands from output in scrollback.
pub(crate) const OSC133_ZONE_START: &str = "\x1b]133;A\x07";
pub(crate) const OSC133_ZONE_END: &str = "\x1b]133;B\x07";
pub(crate) const OSC133_ZONE_FINAL: &str = "\x1b]133;C\x07";

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
pub(crate) fn strip_tool_echoes(
    stdout: &str,
    tools: &[session::ToolCall],
    exec_depth: usize,
) -> String {
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
    let filtered: Vec<&str> = stdout
        .lines()
        .filter(|line| !result_lines.contains(line.trim()))
        .collect();
    // Collapse consecutive blank lines into one and trim leading/trailing
    // blanks, so that blank lines from stripped tool output don't leave gaps.
    let mut out: Vec<&str> = Vec::new();
    let mut prev_blank = false;
    for line in &filtered {
        let is_blank = line.trim().is_empty();
        if is_blank {
            if prev_blank || out.is_empty() {
                continue;
            }
            prev_blank = true;
        } else {
            prev_blank = false;
        }
        out.push(line);
    }
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    out.join("\n")
}

/// Collapse long string literals assigned to `answer["content"]` (or
/// `answer['content']`) in REPL code so the answer text — which is also
/// rendered as a markdown prose card by `push_assistant_message` — does not
/// appear twice in the UI: once as a raw string literal inside the code block
/// and once as formatted markdown below it.
///
/// This is a **data-level** transform on the code string before
/// syntax-highlighting, mirroring `strip_tool_echoes`. It replaces the RHS
/// of any line that assigns to `answer["content"]` / `answer['content']`
/// with a short `"<answer>"` placeholder, preserving the assignment itself so
/// the user can see that the answer was submitted. Lines that do not assign
/// to the answer dict are left untouched, so mixed blocks (real work + answer
/// submission) still show their real code.
pub(crate) fn collapse_answer_assignment(code: &str) -> String {
    // Quick bail: if "answer" doesn't appear at all, return unchanged.
    if !code.contains("answer") {
        return code.to_owned();
    }

    let lines: Vec<&str> = code.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();

        // Check if this line is an answer["content"] / answer['content'] assignment.
        if let Some(eq_pos) = find_answer_content_eq(trimmed) {
            let lhs = &trimmed[..eq_pos];
            let after_eq = trimmed[eq_pos + 1..].trim_start();

            // Triple-quoted string — may span multiple lines.
            if after_eq.starts_with("\"\"\"") || after_eq.starts_with("'''") {
                let quote = &after_eq[..3];
                let rest_after_open = &after_eq[3..];
                if let Some(end) = rest_after_open.find(quote) {
                    // Closing quote is on the same line.
                    let suffix = rest_after_open[end + 3..].trim();
                    out.push(format!(
                        "{} = \"<answer>\"{}",
                        lhs,
                        if suffix.is_empty() {
                            String::new()
                        } else {
                            format!("  # {suffix}")
                        }
                    ));
                    i += 1;
                } else {
                    // Multi-line: consume lines until the closing quote.
                    out.push(format!("{} = \"<answer>\"", lhs));
                    i += 1;
                    while i < lines.len() {
                        if lines[i].contains(quote) {
                            i += 1;
                            break;
                        }
                        i += 1;
                    }
                }
                continue;
            }

            // Single-line single or double quoted string.
            if after_eq.starts_with('"') || after_eq.starts_with('\'') {
                let q = &after_eq[..1];
                if let Some(end) = after_eq[1..].find(q) {
                    let suffix = after_eq[1 + end + 1..].trim();
                    out.push(format!(
                        "{} = \"<answer>\"{}",
                        lhs,
                        if suffix.is_empty() {
                            String::new()
                        } else {
                            format!("  # {suffix}")
                        }
                    ));
                    i += 1;
                    continue;
                }
            }

            // Not a string literal — leave as-is.
            out.push(line.to_owned());
            i += 1;
            continue;
        }

        // Not an answer assignment — keep verbatim.
        out.push(line.to_owned());
        i += 1;
    }

    let result = out.join("\n");
    if code.ends_with('\n') {
        format!("{result}\n")
    } else {
        result
    }
}

/// If `line` is an `answer["content"] = ...` or `answer['content'] = ...`
/// assignment, return the byte index of the `=`. Otherwise `None`.
fn find_answer_content_eq(line: &str) -> Option<usize> {
    let s = line.trim_start();
    let s = s.strip_prefix("answer")?;
    let s = s.trim_start();
    let s = s.strip_prefix('[')?;
    let s = s.trim_start();
    let s = s
        .strip_prefix("\"content\"")
        .or_else(|| s.strip_prefix("'content'"))?;
    let s = s.trim_start();
    let s = s.strip_prefix(']')?;
    let s = s.trim_start();
    let s = s.strip_prefix('=')?;
    Some(line.len() - s.len())
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

/// Flat header line: `{indent(depth)}{title}`. The header is a bare title at
/// the nesting indent (no bullet glyph, no trailing rule, no timestamp).
pub(crate) fn forge_title(depth: usize, title: &str) -> String {
    format!("{}{title}", indent(depth))
}

/// Flat header line + indented result body. `body` lines must already be
/// wrapped to `card_body_width(depth)`. Every body row is inset by the
/// 2-space result gutter (`result_prefix`). Empty body lines collapse to the
/// bare indent. If `body` is empty the header stands alone.
pub(crate) fn render_flat(title: &str, depth: usize, body: &[String]) -> Vec<String> {
    let header = forge_title(depth, title);
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

/// Prepend `prefix` to a row, injecting it *inside* any background-color SGR
/// that opens the row so the prefixed cells carry the background and the fill
/// spans the full terminal width. Rows without a leading bg escape get a
/// plain-space prefix (non-boxed text lines like code, exec bars, and output).
pub(crate) fn prepend_indent(row: &str, prefix: &str) -> String {
    // A background SGR always starts with `[48` (48;2;R;G;B for 24-bit,
    // 48;5;N for 256-colour). `fill_bg` guarantees the bg escape is the very
    // first thing on a boxed row.
    if row.starts_with("\x1b[48") {
        // Find the terminator of the SGR sequence (the first `m`).
        if let Some(close) = row.find('m') {
            // Split after the `m` and insert the prefix *inside* the bg scope
            // so the indent cells are painted with the background colour.
            let (esc, rest) = row.split_at(close + 1);
            return format!("{esc}{prefix}{rest}");
        }
    }
    format!("{prefix}{row}")
}

/// REPL block: a pi-style `Box(1,1,bgFn)` that wraps the code-execution
/// container. The body is a mix of flat text lines (code, exec bar, output —
/// no bg escape) and boxed child card rows (tool/subcall cards, status
/// spinners — already carrying their own bg from `render_boxed`).
///
/// To preserve each child card's own background while applying the REPL bg
/// to flat lines, the function distinguishes the two:
/// - **Boxed child rows** (starting with a bg SGR): the 1-space left pad is
///   injected *inside* the child's own bg via `prepend_indent`, so the child
///   keeps its colour and the pad cell carries the child bg — not the REPL bg.
///   `fill_bg` is NOT called (it would clobber the child's opening bg).
/// - **Flat text rows** (no bg escape): `fill_bg` paints the whole row with
///   the REPL bg, left-padded by 1 space. Blank rows become bg-only fill.
///
/// Top and bottom padding rows are bg-only fills, matching pi's Box padding.
pub(crate) fn render_repl_block(body: &[String], bg: &str, width: usize) -> Vec<String> {
    if body.is_empty() {
        return Vec::new();
    }
    let pad = " ";
    let mut out = Vec::with_capacity(body.len() + 2);
    // Top padding
    out.push(fill_bg(pad, bg, width));
    for line in body {
        if line.is_empty() {
            // Blank line: bg-only fill.
            out.push(fill_bg(pad, bg, width));
        } else if line.starts_with("\x1b[48") {
            // Boxed child card row: inject 1-space pad inside the child's
            // own bg so the child keeps its colour. Do NOT call fill_bg.
            out.push(prepend_indent(line, pad));
        } else {
            // Flat text line: paint with REPL bg.
            let padded = format!("{pad}{line}");
            out.push(fill_bg(&padded, bg, width));
        }
    }
    // Bottom padding
    out.push(fill_bg(pad, bg, width));
    out
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
}

/// Render a boxed block mirroring pi's `Box(1, 1, bgFn)` component: 1-cell
/// padding all around, with the given background applied via `fill_bg`.
///
/// The `lines` are the content (already styled) that goes inside the box.
/// The function prepends a blank bg line, left-pads each content line by one
/// space, applies the bg fill to the full `width`, and appends a trailing
/// blank bg line.
pub(crate) fn render_boxed(lines: &[String], bg: &str, width: usize) -> Vec<String> {
    if lines.is_empty() {
        return Vec::new();
    }
    let pad = " ";
    let mut out = Vec::with_capacity(lines.len() + 2);
    // Top padding
    out.push(fill_bg(pad, bg, width));
    // Content with 1-space left pad
    for line in lines {
        let padded = if line.is_empty() {
            pad.to_owned()
        } else {
            format!("{pad}{line}")
        };
        out.push(fill_bg(&padded, bg, width));
    }
    // Bottom padding
    out.push(fill_bg(pad, bg, width));
    out
}
