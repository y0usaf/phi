use phi_ext::{ToolBody, ToolRenderSpec, title_case_tool_name};

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
/// pattern) is rendered exactly once. Mirrors the interactive `tool_title`:
/// the shape is a projection of the tool's registered render spec; a tool
/// without one gets its bare display name.
pub(crate) fn call_anchor(
    spec: Option<&ToolRenderSpec>,
    name: &str,
    args: &serde_json::Value,
) -> String {
    let display = tool_display_name(spec, name);
    match spec.map(|s| &s.body) {
        // Code-carrying tools (shell/repl shape): the anchor echoes the
        // code's first line, behind the prompt glyph when the spec has one.
        Some(ToolBody::Code { key, prompt, .. }) => {
            let code = tool_arg(args, key);
            if code.trim().is_empty() {
                display
            } else {
                let prompt = prompt
                    .as_deref()
                    .map(|p| format!("{p} "))
                    .unwrap_or_default();
                format!("{display}: {prompt}{}", first_line_trunc(code, 60))
            }
        }
        _ => format!(
            "{display}{}",
            tool_title_suffix(spec, args).unwrap_or_default()
        ),
    }
}

/// `args` with the canonical key(s) already shown in the anchor/title removed,
/// so the Args section never re-states the path/pattern. Tools without anchor
/// args (code-bodied, unknown) are untouched.
pub(crate) fn residual_args(
    spec: Option<&ToolRenderSpec>,
    args: &serde_json::Value,
) -> serde_json::Value {
    let strip: Vec<&str> = spec.map(|s| s.anchor_keys().collect()).unwrap_or_default();
    if strip.is_empty() {
        return args.clone();
    }
    let Some(mut map) = args.as_object().cloned() else {
        return args.clone();
    };
    for key in strip {
        map.remove(key);
    }
    serde_json::Value::Object(map)
}

/// ANSI styling for scrollback event glyphs (gated on a tty in `print_events`).
/// Derived from the unified `phi_tui::Theme` via `default_terminal_colors()` +
/// `generate_theme()` — the same single ANSI→role mapping used by the TUI and
/// the mux compositor.  The CLI can't probe OSC, so it uses the standard ANSI
/// fallback colors.
pub(crate) fn glyph_colors() -> (&'static str, &'static str, &'static str) {
    use std::sync::LazyLock;
    static GLYPHS: LazyLock<(&'static str, &'static str, &'static str)> = LazyLock::new(|| {
        let theme = phi_tui::generate_theme(&phi_tui::default_terminal_colors());
        let active = phi_tui::fg_escape(theme.glyph_active);
        let ok = phi_tui::fg_escape(theme.glyph_ok);
        let err = phi_tui::fg_escape(theme.glyph_err);
        (
            Box::leak(active.into_boxed_str()),
            Box::leak(ok.into_boxed_str()),
            Box::leak(err.into_boxed_str()),
        )
    });
    *GLYPHS
}
pub(crate) const GLYPH_RESET: &str = "\x1b[0m";

/// Leading braille marker for the start of an in-flight top-level action.
pub(crate) fn spin(color: bool) -> String {
    if color {
        let (active, _, _) = glyph_colors();
        format!("{active}\u{283f}{GLYPH_RESET}")
    } else {
        "\u{283f}".to_owned()
    }
}

/// Check mark settling a successfully completed action.
pub(crate) fn check(color: bool) -> String {
    if color {
        let (_, ok, _) = glyph_colors();
        format!("{ok}\u{2713}{GLYPH_RESET}")
    } else {
        "\u{2713}".to_owned()
    }
}

/// Cross mark settling a failed action.
pub(crate) fn cross(color: bool) -> String {
    if color {
        let (_, _, err) = glyph_colors();
        format!("{err}\u{2717}{GLYPH_RESET}")
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

pub(crate) fn tool_display_name(spec: Option<&ToolRenderSpec>, name: &str) -> String {
    match spec {
        Some(spec) => spec.resolved_display_name(),
        None => title_case_tool_name(name),
    }
}

pub(crate) fn tool_arg<'a>(args: &'a serde_json::Value, key: &str) -> &'a str {
    args.get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
}

/// ` — pattern in path` panel-title suffix built from the spec's anchor
/// args (skips absent/skip-listed values). `None` when nothing survives.
pub(crate) fn tool_title_suffix(
    spec: Option<&ToolRenderSpec>,
    args: &serde_json::Value,
) -> Option<String> {
    let spec = spec?;
    let mut parts: Vec<String> = Vec::new();
    for a in &spec.anchor {
        let value = tool_arg(args, &a.key);
        if a.skips(value) {
            continue;
        }
        match &a.prefix {
            Some(prefix) => parts.push(format!("{prefix} {value}")),
            None => parts.push(value.to_owned()),
        }
    }
    (!parts.is_empty()).then(|| format!(" — {}", parts.join(" ")))
}

pub(crate) fn tool_input_label(spec: Option<&ToolRenderSpec>) -> &'static str {
    match spec.map(|s| &s.body) {
        Some(ToolBody::Code {
            prompt: Some(_), ..
        }) => "Command",
        Some(ToolBody::Code { prompt: None, .. }) => "",
        _ => "Args",
    }
}

pub(crate) fn tool_input_lines(
    spec: Option<&ToolRenderSpec>,
    args: &serde_json::Value,
) -> Vec<String> {
    if let Some(ToolBody::Code { key, prompt, .. }) = spec.map(|s| &s.body) {
        let code = tool_arg(args, key);
        return match prompt {
            Some(p) => {
                if code.trim().is_empty() {
                    vec![format!("{p} ")]
                } else {
                    code.lines().map(|line| format!("{p} {line}")).collect()
                }
            }
            None => {
                if code.trim().is_empty() {
                    Vec::new()
                } else {
                    code.lines().map(str::to_owned).collect()
                }
            }
        };
    }
    if args.is_null() {
        Vec::new()
    } else if args.as_object().is_some() {
        // Strip the canonical key(s) already shown in the panel title suffix so
        // the Args section never re-states the path/pattern (anchor-once).
        let residual = residual_args(spec, args);
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

/// Whether the panel gets an Input section (and the matching Output border):
/// code-bodied tools always do (an empty command still reads as `$ `), other
/// tools only when residual args survive anchor stripping.
pub(crate) fn tool_shows_input_section(
    spec: Option<&ToolRenderSpec>,
    args: &serde_json::Value,
) -> bool {
    matches!(spec.map(|s| &s.body), Some(ToolBody::Code { .. }))
        || !tool_input_lines(spec, args).is_empty()
}

pub(crate) fn tool_output_lines(
    spec: Option<&ToolRenderSpec>,
    args: &serde_json::Value,
    text: &str,
) -> (Vec<String>, Vec<String>, bool) {
    if spec.is_some_and(|s| s.exit_code_trailer) {
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
