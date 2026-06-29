/// First non-empty line of `text`, trimmed and truncated to `max` chars with a
/// trailing `…` when either the line or the whole text was cut.
fn first_line_trunc(text: &str, max: usize) -> String {
    let first = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    let short: String = first.chars().take(max).collect();
    if short.len() < first.len() || text.lines().filter(|l| !l.trim().is_empty()).count() > 1 {
        format!("{short}…")
    } else {
        short
    }
}

/// Single-line canonical identity for a tool call, used by the in-flight start
/// line AND the panel title so the load-bearing argument (path / command /
/// pattern) is rendered exactly once. Mirrors the interactive `tool_title`.
pub(crate) fn call_anchor(name: &str, args: &serde_json::Value) -> String {
    let display = tool_display_name(name);
    match name {
        "bash" => {
            let cmd = tool_arg(args, "cmd");
            if cmd.trim().is_empty() {
                display
            } else {
                format!("{display}: $ {}", first_line_trunc(cmd, 60))
            }
        }
        "repl" => {
            let code = args
                .get("code")
                .or_else(|| args.get("input"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            if code.trim().is_empty() {
                display
            } else {
                format!("{display}: {}", first_line_trunc(code, 60))
            }
        }
        _ => format!(
            "{display}{}",
            tool_title_suffix(name, args).unwrap_or_default()
        ),
    }
}

/// `args` with the canonical key(s) already shown in the anchor/title removed,
/// so the Args section never re-states the path/pattern. Bash/repl/unknown are
/// untouched (their body IS the command/code, not a residual).
pub(crate) fn residual_args(name: &str, args: &serde_json::Value) -> serde_json::Value {
    let strip: &[&str] = match name {
        "read_file" | "write_file" | "edit_file" | "ls" => &["path"],
        "grep" | "find" => &["pattern", "path"],
        _ => return args.clone(),
    };
    let Some(mut map) = args.as_object().cloned() else {
        return args.clone();
    };
    for key in strip {
        map.remove(*key);
    }
    serde_json::Value::Object(map)
}

/// ANSI styling for scrollback event glyphs (gated on a tty in `print_events`).
pub(crate) const GLYPH_RESET: &str = "\x1b[0m";
pub(crate) const GLYPH_ACTIVE: &str = "\x1b[38;2;130;160;230m";
pub(crate) const GLYPH_OK: &str = "\x1b[38;2;130;200;140m";
pub(crate) const GLYPH_ERR: &str = "\x1b[38;2;240;120;120m";

/// Leading braille marker for the start of an in-flight top-level action.
pub(crate) fn spin(color: bool) -> String {
    if color {
        format!("{GLYPH_ACTIVE}\u{283f}{GLYPH_RESET}")
    } else {
        "\u{283f}".to_owned()
    }
}

/// Check mark settling a successfully completed action.
pub(crate) fn check(color: bool) -> String {
    if color {
        format!("{GLYPH_OK}\u{2713}{GLYPH_RESET}")
    } else {
        "\u{2713}".to_owned()
    }
}

/// Cross mark settling a failed action.
pub(crate) fn cross(color: bool) -> String {
    if color {
        format!("{GLYPH_ERR}\u{2717}{GLYPH_RESET}")
    } else {
        "\u{2717}".to_owned()
    }
}

pub(crate) fn depth_tag(depth: usize) -> String {
    if depth == 0 {
        String::new()
    } else {
        format!(" d{depth}")
    }
}

pub(crate) fn preview(text: &str, max_lines: usize, verbose: bool) -> String {
    let text = text.trim_end();
    if verbose {
        return text.to_owned();
    }
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= max_lines {
        return text.to_owned();
    }
    let mut out = lines[..max_lines].join("\n");
    out.push_str(&format!("\n... ({} more lines)", lines.len() - max_lines));
    out
}

/// Remove lines from `stdout` that exactly match (after trimming) lines from
/// tool results at `exec_depth`, so that tool output printed via `print(...)`
/// is not duplicated in the REPL stdout preview. Mirrors the TUI's
/// `strip_tool_echoes` logic. Tool results that errored are not stripped (their
/// output may not match stdout). Returns the residual stdout.
pub(crate) fn strip_tool_echoes(stdout: &str, tools: &[ToolEcho], exec_depth: usize) -> String {
    let mut echo_lines: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for tool in tools {
        if tool.depth == exec_depth && !tool.errored {
            for line in tool.result.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    echo_lines.insert(trimmed);
                }
            }
        }
    }
    let filtered: Vec<&str> = stdout
        .lines()
        .filter(|line| !echo_lines.contains(line.trim()))
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
    // Trim trailing blanks.
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    out.join("\n")
}

/// A tool's result echoed into REPL stdout, accumulated for echo stripping.
pub(crate) struct ToolEcho {
    pub result: String,
    pub depth: usize,
    pub errored: bool,
}

pub(crate) fn indent(text: &str) -> String {
    text.lines()
        .map(|line| format!("  | {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn bash_timeout_arg(args: &serde_json::Value) -> Option<u64> {
    args.get("timeout").and_then(|value| match value {
        serde_json::Value::Number(n) => n.as_u64(),
        serde_json::Value::String(s) => s.parse().ok(),
        _ => None,
    })
}

pub(crate) fn parse_bash_result(text: &str) -> (Vec<String>, Option<i64>, Option<u64>) {
    let mut lines = Vec::new();
    let mut exit_code = None;
    let mut timed_out = None;
    for line in text.trim_end().lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed
            .strip_prefix("[exit code ")
            .and_then(|rest| rest.strip_suffix(']'))
            && let Ok(code) = rest.parse::<i64>()
        {
            exit_code = Some(code);
            continue;
        }
        if let Some(rest) = trimmed
            .strip_prefix("Error: command timed out after ")
            .and_then(|rest| rest.strip_suffix('s'))
            && let Ok(secs) = rest.parse::<u64>()
        {
            timed_out = Some(secs);
            continue;
        }
        if trimmed != "(no output)" {
            lines.push(line.to_owned());
        }
    }
    (lines, exit_code, timed_out)
}

pub(crate) fn tool_display_name(name: &str) -> String {
    // The tool is named "bash" but should display as "Shell" in transcripts.
    if name == "bash" {
        return "Shell".to_owned();
    }
    name.split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn tool_arg<'a>(args: &'a serde_json::Value, key: &str) -> &'a str {
    args.get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
}

pub(crate) fn tool_title_suffix(name: &str, args: &serde_json::Value) -> Option<String> {
    match name {
        "read_file" | "edit_file" => {
            let path = tool_arg(args, "path");
            (!path.is_empty()).then(|| format!(" — {path}"))
        }
        "grep" | "find" => {
            let pattern = tool_arg(args, "pattern");
            let path = tool_arg(args, "path");
            if pattern.is_empty() {
                None
            } else if path.is_empty() || path == "." {
                Some(format!(" — {pattern}"))
            } else {
                Some(format!(" — {pattern} in {path}"))
            }
        }
        "ls" => {
            let path = tool_arg(args, "path");
            Some(format!(" — {}", if path.is_empty() { "." } else { path }))
        }
        _ => None,
    }
}

pub(crate) fn tool_shows_args_section(name: &str) -> bool {
    !matches!(name, "read_file" | "grep" | "find" | "ls")
}

pub(crate) fn tool_shows_input_section(name: &str) -> bool {
    name == "bash" || name == "repl" || tool_shows_args_section(name)
}

pub(crate) fn tool_input_label(name: &str) -> &'static str {
    match name {
        "bash" => "Command",
        "repl" => "",
        _ => "Args",
    }
}

pub(crate) fn tool_input_lines(name: &str, args: &serde_json::Value) -> Vec<String> {
    if name == "bash" {
        let cmd = args
            .get("cmd")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        if cmd.trim().is_empty() {
            vec!["$ ".to_owned()]
        } else {
            cmd.lines().map(|line| format!("$ {line}")).collect()
        }
    } else if name == "repl" {
        let code = args
            .get("code")
            .or_else(|| args.get("input"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        if code.trim().is_empty() {
            Vec::new()
        } else {
            code.lines().map(str::to_owned).collect()
        }
    } else if !tool_shows_args_section(name) || args.is_null() {
        Vec::new()
    } else if args.as_object().is_some() {
        // Strip the canonical key(s) already shown in the panel title suffix so
        // the Args section never re-states the path/pattern (anchor-once).
        let residual = residual_args(name, args);
        if residual.as_object().is_some_and(|m| m.is_empty()) {
            Vec::new()
        } else {
            serde_json::to_string_pretty(&residual)
                .unwrap_or_else(|_| residual.to_string())
                .lines()
                .map(str::to_owned)
                .collect()
        }
    } else {
        let text = match args {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        if text.trim().is_empty() {
            Vec::new()
        } else {
            text.lines().map(str::to_owned).collect()
        }
    }
}

pub(crate) fn tool_output_lines(
    name: &str,
    args: &serde_json::Value,
    text: &str,
) -> (Vec<String>, Vec<String>, bool) {
    if name == "bash" {
        let timeout = bash_timeout_arg(args);
        let (lines, exit_code, timed_out) = parse_bash_result(text);
        let ok = exit_code.unwrap_or(0) == 0 && timed_out.is_none();
        let mut footer = Vec::new();
        if let Some(code) = exit_code.filter(|code| *code != 0) {
            footer.push(format!("Command exited with code {code}"));
        }
        if let Some(secs) = timed_out {
            footer.push(format!("Command timed out after {secs}s"));
        }
        if let Some(secs) = timeout.or(timed_out) {
            footer.push(format!("⟦Timeout: {secs}s⟧"));
        }
        (lines, footer, ok)
    } else {
        let lines: Vec<String> = if text.trim().is_empty() {
            Vec::new()
        } else {
            text.trim_end().lines().map(str::to_owned).collect()
        };
        (lines, Vec::new(), true)
    }
}
