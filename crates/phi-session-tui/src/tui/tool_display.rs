//! Tool identity, title formatting, and status text helpers.

use phi_ai::Model;
use phi_ext::{AnchorStyle, ChromeSpec, ResultLang, ToolBody, ToolRenderSpec};

use super::palette::InlinePalette;
use super::*;

pub(crate) fn subcall_title(model: &str, prompt_preview: &str, p: &InlinePalette) -> String {
    let prompt_preview = prompt_preview
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "{subcall}{model}{RESET} {ghost}←{RESET} {strfg}\"{prompt_preview}\"{RESET}",
        subcall = &p.subcall_accent,
        ghost = &p.text_ghost,
        strfg = &p.string_fg,
    )
}

/// Format a running tool/subcall status line with an ANSI-aware shimmer sweep
/// applied to the label, mirroring claude-code's glimmer shimmer for in-flight
/// tool headers. The spinner glyph carries the working color (`spinner_fg`)
/// but is not shimmered.
pub(crate) fn running_status_text_shimmer(
    label: &str,
    spinner: &str,
    spinner_fg: &str,
    shimmer: phi_tui::Rgb,
    elapsed_ms: u64,
) -> String {
    let shimmed = shimmer_ansi(label, shimmer, elapsed_ms);
    format!("{spinner_fg}{spinner}{RESET} {shimmed}")
}

pub(crate) fn failed_status_text(
    label: &str,
    duration_secs: Option<f64>,
    detail: Option<&str>,
    chrome: &ChromeSpec,
    p: &InlinePalette,
) -> String {
    let duration = duration_secs
        .map(|secs| format!(" {numfg}{secs:.1}s{RESET}", numfg = &p.num_fg))
        .unwrap_or_default();
    let detail = detail
        .filter(|d| !d.trim().is_empty())
        .map(|d| format!(" {ghost}—{RESET} {d}", ghost = &p.text_ghost))
        .unwrap_or_default();
    format!(
        "{}{}{RESET} {label}{duration}{detail}",
        p.error_fg, chrome.glyph_failed
    )
}

/// Canonical identity for a tool call — the single source of truth shared by
/// the in-flight status line and the committed card header. `anchor` is the
/// one-line form (verb + defining argument, e.g. `read src/parser.rs`,
/// `$ cargo check`); `body` is the multi-line detail folded under it (full
/// highlighted command code, a long/nested unknown-tool arg). The shape is
/// a projection of the tool's registered [`ToolRenderSpec`] (WS1.3a) — this
/// file holds no per-tool-name knowledge; a tool without a spec gets the
/// generic `name key=value` form. `tool_title` below is a thin projection
/// over this.
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

pub(crate) fn tool_identity(
    spec: Option<&ToolRenderSpec>,
    name: &str,
    args: &serde_json::Value,
    chrome: &ChromeSpec,
    p: &InlinePalette,
) -> Identity {
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
    // Every anchor is composed from a styled verb segment and an optional
    // styled args segment through the chrome's `anchor_format` template
    // (`{verb} {args}` stock, `{verb}({args})` claude-code); an argless
    // call collapses to the bare verb.
    let compose = |verb: &str, args_seg: Option<String>| {
        chrome.format_anchor(
            &format!("{tool}{verb}{RESET}", tool = &p.tool_title_fg),
            args_seg.as_deref().filter(|s| !s.is_empty()),
        )
    };
    // Fallback anchor (unknown tools, and `repl` which has no dedicated one):
    // `name key=value …` or bare `name` — exactly what `tool_title` produced.
    let fallback_anchor = || {
        let args_seg = match args.as_object() {
            Some(map) if !map.is_empty() => {
                let pairs: Vec<String> = map
                    .iter()
                    .map(|(key, value)| match value {
                        serde_json::Value::String(s) => format!("{key}={}", one_line(s)),
                        other => format!("{key}={other}"),
                    })
                    .collect();
                Some(format!(
                    "{tert}{}{RESET}",
                    pairs.join(" "),
                    tert = &p.text_tertiary,
                ))
            }
            _ => None,
        };
        compose(name, args_seg)
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
                .map(|line| format!("{}{line}{RESET}", &p.text_tertiary))
                .collect()
        } else {
            Vec::new()
        }
    };
    let (anchor, body): (String, Vec<String>) = match spec {
        // Code-bodied tool (shell/repl shape): the anchor carries the code's
        // truncated first line; the body carries the full text only when it
        // is multi-line (anchor-once — the canonical code is shown exactly
        // once, not in both the anchor and a body line).
        Some(ToolRenderSpec {
            verb,
            body: ToolBody::Code { key, lang, prompt },
            ..
        }) => {
            let code = arg(key);
            let multiline = code.lines().count() > 1;
            let anchor = match verb {
                // Multi-line: the truncated first line, unhighlighted — the
                // full highlighted command is the body.
                Some(verb) if multiline => compose(
                    verb,
                    Some(format!(
                        "{tert}{}{RESET}",
                        one_line(code),
                        tert = &p.text_tertiary,
                    )),
                ),
                Some(verb) if code.trim().is_empty() => compose(verb, None),
                Some(verb) => {
                    let echoed = one_line(code);
                    let lit = highlight_code(&echoed, lang.as_deref(), &p.markdown)
                        .into_iter()
                        .next()
                        .unwrap_or(echoed);
                    compose(verb, Some(format!("{lit}{RESET}")))
                }
                // No verb (repl): keep the fallback `name key=<first line>…`
                // anchor form.
                None => fallback_anchor(),
            };
            let body = if code.trim().is_empty() || !multiline {
                Vec::new()
            } else {
                highlight_code(code, lang.as_deref(), &p.markdown)
                    .into_iter()
                    .map(|line| match prompt {
                        Some(pr) => format!("{tool}{pr}{RESET} {line}", tool = &p.tool_title_fg),
                        None => line,
                    })
                    .collect()
            };
            (anchor, body)
        }
        // Anchored tool (read/grep/ls shape): verb + styled canonical args.
        Some(spec) if spec.verb.is_some() || !spec.anchor.is_empty() => {
            let verb = spec.verb.as_deref().unwrap_or(name);
            let mut segments: Vec<String> = Vec::new();
            for a in &spec.anchor {
                let raw = arg(&a.key);
                if a.skips(raw) {
                    continue;
                }
                let value = match a.style {
                    // Paths are shown in full; patterns/plain values are
                    // truncated to the anchor's one-line budget.
                    AnchorStyle::Path => raw.to_owned(),
                    AnchorStyle::Pattern | AnchorStyle::Plain => one_line(raw),
                };
                let color = match a.style {
                    AnchorStyle::Path => &p.path_fg,
                    AnchorStyle::Pattern => &p.string_fg,
                    AnchorStyle::Plain => &p.text_tertiary,
                };
                let rendered = match &a.prefix {
                    Some(prefix) => format!("{prefix} {value}"),
                    None => value,
                };
                segments.push(format!("{color}{rendered}{RESET}"));
            }
            let args_seg = (!segments.is_empty()).then(|| segments.join(" "));
            let body = match &spec.body {
                ToolBody::Args => fallback_body(),
                _ => Vec::new(),
            };
            (compose(verb, args_seg), body)
        }
        Some(spec) => {
            let body = match &spec.body {
                ToolBody::Args => fallback_body(),
                _ => Vec::new(),
            };
            (fallback_anchor(), body)
        }
        None => (fallback_anchor(), fallback_body()),
    };
    Identity { anchor, body }
}

/// One-line title for a tool block. Thin projection over [`tool_identity`].
pub(crate) fn tool_title(
    spec: Option<&ToolRenderSpec>,
    name: &str,
    args: &serde_json::Value,
    chrome: &ChromeSpec,
    p: &InlinePalette,
) -> String {
    tool_identity(spec, name, args, chrome, p).anchor
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

/// Syntax-highlight hint for a tool's result preview, from the spec's
/// [`ResultLang`] (e.g. `read_file` results highlighted by the file's
/// language).
pub(crate) fn tool_preview_lang(
    spec: Option<&ToolRenderSpec>,
    args: &serde_json::Value,
) -> Option<String> {
    match &spec?.result_lang {
        ResultLang::None => None,
        ResultLang::Fixed { lang } => Some(lang.clone()),
        ResultLang::PathArg { key } => args
            .get(key)
            .and_then(serde_json::Value::as_str)
            .and_then(lang_hint_for_path)
            .map(str::to_owned),
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
