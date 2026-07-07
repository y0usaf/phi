//! The transcript line vocabulary (PLAN.md WS3.2): every status line a
//! frontend prints is built here, so the one-shot stderr renderer and the
//! inline TUI speak identically. forgecode-minimal: one `●` glyph per
//! line, dim secondary text, red errors, no timestamps, no borders; the
//! whole vocabulary uses two SGR attributes (dim, red).

use serde_json::Value;

/// Longest argument summary shown on a `●` line.
const SUMMARY_MAX_CHARS: usize = 80;
/// Verbose output block: lines shown before the `… +N lines` marker.
const OUTPUT_MAX_LINES: usize = 10;

pub(crate) fn dim(s: &str, color: bool) -> String {
    if color {
        format!("\x1b[2m{s}\x1b[0m")
    } else {
        s.to_owned()
    }
}

pub(crate) fn red(s: &str, color: bool) -> String {
    if color {
        format!("\x1b[31m{s}\x1b[0m")
    } else {
        s.to_owned()
    }
}

/// One line per tool call: `● name summary`, summary dim. edit/write get
/// a compact diff stat computed from the arguments (the line prints before
/// the call runs; for exact-match edits and whole-file writes the
/// arguments determine the change).
pub(crate) fn tool_line(name: &str, arguments: &Value, color: bool) -> String {
    let summary = match name {
        "edit" => edit_summary(arguments),
        "write" => write_summary(arguments),
        _ => None,
    }
    .unwrap_or_else(|| summarize(arguments));
    if summary.is_empty() {
        format!("● {name}")
    } else {
        format!("● {name} {}", dim(&summary, color))
    }
}

/// `● ERROR: <first line>` in red — the only tool output shown when not
/// verbose.
pub(crate) fn error_line(content: &str, color: bool) -> String {
    let first = content.lines().next().unwrap_or("").trim();
    red(&format!("● ERROR: {first}"), color)
}

/// Verbose mode: the tool result under its `●` line, dim and indented.
/// Head-capped — the tool layer already truncates the data; this cap is
/// about scrollback.
pub(crate) fn output_block(content: &str, color: bool) -> Vec<String> {
    let lines: Vec<&str> = content.lines().collect();
    let mut out: Vec<String> = lines
        .iter()
        .take(OUTPUT_MAX_LINES)
        .map(|line| dim(&format!("  {}", line.trim_end()), color))
        .collect();
    if lines.len() > OUTPUT_MAX_LINES {
        out.push(dim(
            &format!("  … +{} lines", lines.len() - OUTPUT_MAX_LINES),
            color,
        ));
    }
    out
}

pub(crate) fn interrupted_line(color: bool) -> String {
    dim("● interrupted", color)
}

pub(crate) fn retry_line(attempt: u32, max: u32, color: bool) -> String {
    dim(&format!("● retrying ({attempt}/{max})"), color)
}

/// `path +added -removed` for an edit call, summed over its edits.
fn edit_summary(args: &Value) -> Option<String> {
    let path = args.get("path")?.as_str()?;
    let edits = args.get("edits")?.as_array()?;
    let (mut added, mut removed) = (0, 0);
    for edit in edits {
        let old = edit.get("old_text").and_then(Value::as_str).unwrap_or("");
        let new = edit.get("new_text").and_then(Value::as_str).unwrap_or("");
        let (a, r) = diff_stat(old, new);
        added += a;
        removed += r;
    }
    Some(format!("{path} +{added} -{removed}"))
}

/// `path +lines` for a write call (the previous contents, if any, are
/// unknown here — no minus count).
fn write_summary(args: &Value) -> Option<String> {
    let path = args.get("path")?.as_str()?;
    let content = args.get("content")?.as_str()?;
    Some(format!("{path} +{}", content.lines().count()))
}

/// Line-level diff stat between two fragments: common prefix and suffix
/// lines drop out, the remainders count as added/removed. A changed line
/// counts once on each side.
fn diff_stat(old: &str, new: &str) -> (usize, usize) {
    let old: Vec<&str> = old.lines().collect();
    let new: Vec<&str> = new.lines().collect();
    let mut prefix = 0;
    while prefix < old.len() && prefix < new.len() && old[prefix] == new[prefix] {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < old.len() - prefix
        && suffix < new.len() - prefix
        && old[old.len() - 1 - suffix] == new[new.len() - 1 - suffix]
    {
        suffix += 1;
    }
    (new.len() - prefix - suffix, old.len() - prefix - suffix)
}

/// Generic argument summary: the command or path when the tool has one,
/// compact JSON otherwise; first line only, capped for the terminal.
fn summarize(arguments: &Value) -> String {
    let text = ["command", "path"]
        .iter()
        .find_map(|k| arguments.get(k).and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_else(|| serde_json::to_string(arguments).unwrap_or_default());
    let line = text.lines().next().unwrap_or("").trim();
    let mut out: String = line.chars().take(SUMMARY_MAX_CHARS).collect();
    if line.chars().count() > SUMMARY_MAX_CHARS {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_line_golden_with_color() {
        assert_eq!(
            tool_line("bash", &json!({ "command": "ls -la" }), true),
            "● bash \x1b[2mls -la\x1b[0m"
        );
    }

    #[test]
    fn tool_line_golden_plain() {
        assert_eq!(
            tool_line("bash", &json!({ "command": "ls -la" }), false),
            "● bash ls -la"
        );
        assert_eq!(
            tool_line("read", &json!({ "path": "src/main.rs" }), false),
            "● read src/main.rs"
        );
    }

    #[test]
    fn edit_line_shows_diff_stat() {
        let args = json!({
            "path": "src/main.rs",
            "edits": [
                { "old_text": "a\nb\nc", "new_text": "a\nx\ny\nc" },
                { "old_text": "gone", "new_text": "" }
            ]
        });
        // edit 1: b → x,y = +2 -1; edit 2: gone → (empty) = +0 -1.
        assert_eq!(tool_line("edit", &args, false), "● edit src/main.rs +2 -2");
    }

    #[test]
    fn write_line_shows_added_lines() {
        let args = json!({ "path": "a/b.txt", "content": "one\ntwo\nthree" });
        assert_eq!(tool_line("write", &args, false), "● write a/b.txt +3");
    }

    #[test]
    fn malformed_edit_args_fall_back_to_generic_summary() {
        assert_eq!(
            tool_line("edit", &json!({ "path": "x.txt" }), false),
            "● edit x.txt"
        );
        assert_eq!(
            tool_line("edit", &json!({ "n": 1 }), false),
            "● edit {\"n\":1}"
        );
    }

    #[test]
    fn empty_summary_leaves_a_bare_glyph_line() {
        assert_eq!(
            tool_line("bash", &json!({ "command": "" }), false),
            "● bash"
        );
    }

    #[test]
    fn summary_prefers_command_then_path() {
        assert_eq!(summarize(&json!({ "command": "ls", "path": "x" })), "ls");
        assert_eq!(summarize(&json!({ "path": "src/main.rs" })), "src/main.rs");
    }

    #[test]
    fn summary_falls_back_to_json_and_truncates() {
        assert_eq!(summarize(&json!({ "n": 1 })), "{\"n\":1}");
        let long = "x".repeat(200);
        let out = summarize(&json!({ "command": long }));
        assert_eq!(out.chars().count(), SUMMARY_MAX_CHARS + 1);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn summary_uses_first_line_only() {
        assert_eq!(summarize(&json!({ "command": "echo a\necho b" })), "echo a");
    }

    #[test]
    fn error_line_golden() {
        assert_eq!(
            error_line("boom\ndetail", true),
            "\x1b[31m● ERROR: boom\x1b[0m"
        );
        assert_eq!(error_line("  padded  \nrest", false), "● ERROR: padded");
    }

    #[test]
    fn output_block_indents_and_dims() {
        assert_eq!(output_block("one\ntwo  ", false), vec!["  one", "  two"]);
        assert_eq!(output_block("x", true), vec!["\x1b[2m  x\x1b[0m"]);
    }

    #[test]
    fn output_block_caps_with_a_marker() {
        let content = (1..=14)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let block = output_block(&content, false);
        assert_eq!(block.len(), 11);
        assert_eq!(block[9], "  10");
        assert_eq!(block[10], "  … +4 lines");
    }

    #[test]
    fn output_block_empty_content_is_empty() {
        assert!(output_block("", false).is_empty());
    }

    #[test]
    fn status_lines_golden() {
        assert_eq!(interrupted_line(false), "● interrupted");
        assert_eq!(interrupted_line(true), "\x1b[2m● interrupted\x1b[0m");
        assert_eq!(retry_line(1, 3, false), "● retrying (1/3)");
    }

    #[test]
    fn diff_stat_drops_common_prefix_and_suffix() {
        assert_eq!(diff_stat("a\nb\nc", "a\nx\nc"), (1, 1));
        assert_eq!(diff_stat("a", "a\nb"), (1, 0));
        assert_eq!(diff_stat("a\nb", "a"), (0, 1));
        assert_eq!(diff_stat("same", "same"), (0, 0));
        assert_eq!(diff_stat("", "x\ny"), (2, 0));
    }
}
