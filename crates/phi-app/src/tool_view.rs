use phi_rlm::LimitKind;

/// Compact one-line `key=value` summary of a tool call's arguments.
pub(crate) fn tool_args_preview(args: &serde_json::Value) -> String {
    let Some(map) = args.as_object() else {
        return String::new();
    };
    map.iter()
        .map(|(key, value)| {
            let text = match value {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            let flat = text.replace('\n', "\\n");
            let short: String = flat.chars().take(60).collect();
            let ellipsis = if short.len() < flat.len() { "…" } else { "" };
            format!("{key}={short}{ellipsis}")
        })
        .collect::<Vec<_>>()
        .join(" ")
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

pub(crate) fn limit_name(limit: LimitKind) -> &'static str {
    match limit {
        LimitKind::Budget => "budget",
        LimitKind::Timeout => "timeout",
        LimitKind::Tokens => "tokens",
        LimitKind::Errors => "errors",
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
        "repl" => "Input",
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
    } else if let Some(map) = args.as_object() {
        if map.is_empty() {
            Vec::new()
        } else {
            serde_json::to_string_pretty(args)
                .unwrap_or_else(|_| args.to_string())
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
