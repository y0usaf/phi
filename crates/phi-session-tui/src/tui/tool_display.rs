//! Tool identity, title formatting, and status text helpers.

use phi_ai::Model;

use super::palette::InlinePalette;
use super::*;

pub(crate) fn subcall_title(model: &str, prompt_preview: &str, p: &InlinePalette) -> String {
    let prompt_preview = prompt_preview
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "{subcall}{model}{RESET} {DIM}←{RESET} {strfg}\"{prompt_preview}\"{RESET}",
        subcall = &p.subcall_accent,
        strfg = &p.string_fg,
    )
}

pub(crate) fn running_status_text(label: &str, spinner: &str) -> String {
    format!("{spinner} {label}")
}

#[allow(dead_code)] // reserved for a success-status state the status machine does not yet emit
pub(crate) fn success_status_text(label: &str, duration_secs: f64, p: &InlinePalette) -> String {
    format!(
        "{}✓{RESET} {label} {numfg}{duration_secs:.1}s{RESET}",
        &p.chrome_cost_fg,
        numfg = &p.num_fg,
    )
}

pub(crate) fn failed_status_text(
    label: &str,
    duration_secs: Option<f64>,
    detail: Option<&str>,
    p: &InlinePalette,
) -> String {
    let duration = duration_secs
        .map(|secs| format!(" {numfg}{secs:.1}s{RESET}", numfg = &p.num_fg))
        .unwrap_or_default();
    let detail = detail
        .filter(|d| !d.trim().is_empty())
        .map(|d| format!(" {DIM}—{RESET} {d}"))
        .unwrap_or_default();
    format!("{}✗{RESET} {label}{duration}{detail}", p.error_fg)
}

/// Canonical identity for a tool call — the single source of truth shared by
/// the in-flight status line and the committed card header. `anchor` is the
/// one-line form (verb + defining argument, e.g. `read src/parser.rs`,
/// `$ cargo check`); `body` is the multi-line detail folded under it (full
/// highlighted bash command, repl code, a long/nested unknown-tool arg).
/// `tool_title` below is a thin projection over this — never duplicate the
/// per-tool match.
///
/// Anchor-once invariant: the canonical argument (path/command/pattern/value)
/// is shown **once** across the card — in the anchor when it fits, in the body
/// when it's richer than a one-line anchor can carry. So bash/repl keep a body
/// only for multi-line command/code (the anchor is the truncated first line);
/// single-line bash/repl have an empty body (the anchor already shows them).
/// read/write/edit/grep/find/ls have an empty body — the identity IS the
/// anchor. Unknown tools keep a pretty-printed body only when a value is
/// multi-line, long, or nested; otherwise the `key=value` anchor carries it.
pub(crate) struct Identity {
    pub(crate) anchor: String,
    pub(crate) body: Vec<String>,
}

pub(crate) fn tool_identity(name: &str, args: &serde_json::Value, p: &InlinePalette) -> Identity {
    let arg = |key: &str| tool_arg(args, key);
    let one_line = |text: &str| {
        let flat = text.lines().next().unwrap_or("").trim().to_owned();
        let short: String = flat.chars().take(80).collect();
        if short.len() < flat.len() || text.lines().count() > 1 {
            format!("{short}…")
        } else {
            short
        }
    };
    let in_path = |path: &str| {
        if path.is_empty() || path == "." {
            String::new()
        } else {
            format!(" in {path}")
        }
    };
    // Fallback anchor (unknown tools, and `repl` which has no dedicated one):
    // `name key=value …` or bare `name` — exactly what `tool_title` produced.
    let fallback_anchor = || match args.as_object() {
        Some(map) if !map.is_empty() => {
            let pairs: Vec<String> = map
                .iter()
                .map(|(key, value)| match value {
                    serde_json::Value::String(s) => format!("{key}={}", one_line(s)),
                    other => format!("{key}={other}"),
                })
                .collect();
            format!(
                "{tool}{name}{RESET} {DIM}{}{RESET}",
                pairs.join(" "),
                tool = &p.tool_accent,
            )
        }
        _ => format!("{tool}{name}{RESET}", tool = &p.tool_accent),
    };
    // Fallback body: a pretty-print of the args shown only when it carries
    // detail the one-line anchor can't — a value that is multi-line, longer
    // than one line can fit, or nested (object/array). When every value fits
    // the `key=value` anchor un-truncated, the body is empty (anchor-once —
    // the same invariant read/grep/ls/bash honor: the canonical arg appears
    // once total, the full text shown once when it's richer than the anchor).
    let fallback_body = || {
        if args.is_null() || args.as_object().is_some_and(|m| m.is_empty()) {
            return Vec::new();
        }
        // For an object, the anchor already shows every `key=value`, so the
        // body earns a row only when some value is richer than one line. For a
        // bare (non-object) arg the anchor is just the tool name — the value is
        // not shown there — so the body carries it whenever it's non-empty.
        let needs_body = match args {
            serde_json::Value::Object(map) => map.values().any(value_richer_than_one_line),
            other => !is_empty_value(other),
        };
        if needs_body {
            serde_json::to_string_pretty(args)
                .unwrap_or_else(|_| args.to_string())
                .lines()
                .map(|line| format!("{DIM}{line}{RESET}"))
                .collect()
        } else {
            Vec::new()
        }
    };
    let (anchor, body): (String, Vec<String>) = match name {
        "bash" => {
            let cmd = arg("cmd");
            let multiline = cmd.lines().count() > 1;
            let anchor = if multiline {
                // Multi-line: anchor is the truncated first line; the full
                // command is the body, so the canonical content is shown once.
                format!(
                    "{tool}${RESET} {DIM}{}{RESET}",
                    one_line(cmd),
                    tool = &p.tool_accent,
                )
            } else {
                let echoed = one_line(cmd);
                let lit = highlight_code(&echoed, Some("bash"), &p.markdown)
                    .into_iter()
                    .next()
                    .unwrap_or(echoed);
                format!("{tool}${RESET} {lit}{RESET}", tool = &p.tool_accent)
            };
            // The body carries the full multi-line command. For a single-line
            // command the anchor already shows it — emitting a body line
            // would duplicate the canonical content (anchor-once), so the
            // body is reserved for the multi-line case (and empty cmd).
            let body = if cmd.trim().is_empty() {
                vec![format!("{tool}${RESET}", tool = &p.tool_accent)]
            } else if multiline {
                highlight_code(cmd, Some("bash"), &p.markdown)
                    .into_iter()
                    .map(|line| format!("{tool}${RESET} {line}", tool = &p.tool_accent))
                    .collect()
            } else {
                Vec::new()
            };
            (anchor, body)
        }
        "read_file" | "write_file" | "edit_file" => {
            let verb = match name {
                "read_file" => "read",
                "write_file" => "write",
                _ => "edit",
            };
            let path = arg("path");
            (
                format!(
                    "{tool}{verb}{RESET} {pathfg}{}{RESET}",
                    path,
                    tool = &p.tool_accent,
                    pathfg = &p.path_fg,
                ),
                Vec::new(),
            )
        }
        "grep" | "find" => {
            let verb = if name == "grep" { "grep" } else { "find" };
            let pattern = arg("pattern");
            let path = arg("path");
            let anchor = format!(
                "{tool}{verb}{RESET} {strfg}{}{RESET}{pathfg}{}{RESET}",
                one_line(pattern),
                in_path(path),
                tool = &p.tool_accent,
                strfg = &p.string_fg,
                pathfg = &p.path_fg,
            );
            (anchor, Vec::new())
        }
        "ls" => {
            let path = arg("path");
            let anchor = format!(
                "{tool}ls{RESET} {pathfg}{}{RESET}",
                path,
                tool = &p.tool_accent,
                pathfg = &p.path_fg,
            )
            .trim_end()
            .to_owned();
            (anchor, Vec::new())
        }
        "repl" => {
            // No dedicated one-line anchor (keeps the fallback form `repl
            // code=<first line>…`); the body carries the full repl code. A
            // single-line code value is already in the anchor, so the body is
            // reserved for the multi-line case (anchor-once — the canonical
            // code is shown once, not in both the anchor and a body line).
            let code = args
                .get("code")
                .or_else(|| args.get("input"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let body = if code.trim().is_empty() || code.lines().count() <= 1 {
                Vec::new()
            } else {
                highlight_code(code, Some("repl"), &p.markdown)
            };
            (fallback_anchor(), body)
        }
        _ => (fallback_anchor(), fallback_body()),
    };
    Identity { anchor, body }
}

/// One-line title for a tool block. Thin projection over [`tool_identity`].
pub(crate) fn tool_title(name: &str, args: &serde_json::Value, p: &InlinePalette) -> String {
    tool_identity(name, args, p).anchor
}

pub(crate) fn lang_hint_for_path(path: &str) -> Option<&'static str> {
    let path = Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path);
    if path.eq_ignore_ascii_case("dockerfile") {
        return Some("docker");
    }
    let ext = path
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())?;
    match ext.as_str() {
        "rs" => Some("rust"),
        "py" => Some("python"),
        "js" | "jsx" | "mjs" | "cjs" => Some("javascript"),
        "ts" | "tsx" => Some("typescript"),
        "sh" | "bash" | "zsh" | "fish" => Some("bash"),
        "json" => Some("json"),
        "toml" => Some("toml"),
        "yaml" | "yml" => Some("yaml"),
        "md" => Some("markdown"),
        "html" | "htm" => Some("html"),
        "css" | "scss" => Some("css"),
        "sql" => Some("sql"),
        "xml" => Some("xml"),
        _ => None,
    }
}

pub(crate) fn tool_preview_lang(name: &str, args: &serde_json::Value) -> Option<&'static str> {
    match name {
        "read_file" => args
            .get("path")
            .and_then(serde_json::Value::as_str)
            .and_then(lang_hint_for_path),
        _ => None,
    }
}

fn tool_arg<'a>(args: &'a serde_json::Value, key: &str) -> &'a str {
    args.get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
}

/// Whether a value carries detail the one-line `key=value` anchor cannot show:
/// a multi-line string, a >80-char string, or any nested object/array. Used to
/// decide whether the fallback body (full pretty-printed arg) earns a row, so
/// the canonical value is shown once at full fidelity only when the anchor
/// truncates it (anchor-once).
pub(crate) fn value_richer_than_one_line(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::String(s) => {
            let flat = s.lines().next().unwrap_or("");
            flat.chars().take(80).collect::<String>().chars().count() < flat.chars().count()
                || s.lines().count() > 1
        }
        serde_json::Value::Object(_) | serde_json::Value::Array(_) => true,
        _ => false,
    }
}

/// Trivially-empty value: null, empty string, empty array/object.
pub(crate) fn is_empty_value(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::Null => true,
        serde_json::Value::String(s) => s.is_empty(),
        serde_json::Value::Array(a) => a.is_empty(),
        serde_json::Value::Object(m) => m.is_empty(),
        _ => false,
    }
}

pub(crate) fn format_tokens(count: u64) -> String {
    match count {
        0..=999 => count.to_string(),
        1_000..=9_999 => format!("{:.1}k", count as f64 / 1_000.0),
        10_000..=999_999 => format!("{}k", count / 1_000),
        _ => format!("{:.1}M", count as f64 / 1_000_000.0),
    }
}

pub(crate) fn format_rate(rate: f64) -> String {
    if rate == 0.0 {
        "$0/M".to_owned()
    } else if rate >= 10.0 {
        format!("${rate:.0}/M")
    } else if rate >= 1.0 {
        format!("${rate:.2}/M")
    } else {
        format!("${rate:.4}/M")
    }
}

pub(crate) fn format_model_pricing(model: &Model) -> String {
    let mut parts = vec![
        format!("in {}", format_rate(model.cost.input)),
        format!("out {}", format_rate(model.cost.output)),
    ];
    if model.cost.cache_write > 0.0 {
        parts.push(format!("cache+ {}", format_rate(model.cost.cache_write)));
    }
    if model.cost.cache_read > 0.0 {
        parts.push(format!("cache↺ {}", format_rate(model.cost.cache_read)));
    }
    parts.join(" · ")
}

