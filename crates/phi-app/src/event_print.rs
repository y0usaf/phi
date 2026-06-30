use std::collections::HashMap;
use std::io::IsTerminal;

use phi_rlm::RlmEvent;
use phi_session::config::ToolsSettings;

use crate::render::render_tool_panel;
use crate::tool_view::{
    ToolEcho, call_anchor, check, cross, depth_tag, indent, preview, spin, strip_tool_echoes,
};

pub(crate) async fn print_events(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<RlmEvent>,
    verbose: bool,
    max_iterations: usize,
    tools: ToolsSettings,
) {
    let color = std::io::stderr().is_terminal();
    let mut open_tool_args: HashMap<u64, serde_json::Value> = HashMap::new();
    let mut tool_echoes: Vec<ToolEcho> = Vec::new();
    while let Some(event) = rx.recv().await {
        match event {
            RlmEvent::IterationStart { depth, iteration } => {
                if depth == 0 {
                    eprintln!("\n[turn {}]", iteration + 1);
                } else {
                    eprintln!(
                        "\n[turn {}/{max_iterations}]{}",
                        iteration + 1,
                        depth_tag(depth)
                    );
                }
            }
            RlmEvent::CodeExecStart { code, .. } => {
                tool_echoes.clear();
                eprintln!("{} exec", spin(color));
                if tools.repl_max_lines > 0 {
                    eprintln!("{}", indent(&preview(&code, tools.repl_max_lines, verbose)));
                }
            }
            RlmEvent::CodeExecEnd {
                depth,
                stdout,
                stderr,
                duration,
                ..
            } => {
                let glyph = if stderr.trim().is_empty() {
                    check(color)
                } else {
                    cross(color)
                };
                if (duration.as_secs_f64() * 10.0).round() <= 0.0 {
                    eprintln!("  {glyph} exec");
                } else {
                    eprintln!("  {glyph} exec • {:.1}s", duration.as_secs_f64());
                }
                let stdout = strip_tool_echoes(&stdout, &tool_echoes, depth);
                if tools.tool_max_lines > 0 && !stdout.trim().is_empty() {
                    eprintln!(
                        "{}",
                        indent(&preview(&stdout, tools.tool_max_lines, verbose))
                    );
                }
                if tools.tool_max_lines > 0 && !stderr.trim().is_empty() {
                    eprintln!(
                        "{}",
                        indent(&preview(&stderr, tools.tool_max_lines, verbose))
                    );
                }
            }
            RlmEvent::SubcallStart {
                depth,
                model,
                prompt_preview,
            } => {
                eprintln!(
                    "{} subcall {model}{} {prompt_preview}",
                    spin(color),
                    depth_tag(depth)
                );
            }
            RlmEvent::SubcallEnd {
                model,
                duration,
                error,
                ..
            } => match error {
                Some(error) => {
                    eprintln!("  {} subcall {model} failed: {error}", cross(color))
                }
                None => eprintln!(
                    "  {} subcall {model} {:.1}s",
                    check(color),
                    duration.as_secs_f64()
                ),
            },
            RlmEvent::ToolStart {
                call_id,
                depth,
                name,
                args,
            } => {
                open_tool_args.insert(call_id, args.clone());
                if tools.gap > 0 {
                    eprintln!();
                }
                eprintln!(
                    "{} {}{}",
                    spin(color),
                    call_anchor(&name, &args),
                    depth_tag(depth)
                );
            }
            RlmEvent::ToolEnd {
                call_id,
                name,
                result,
                error,
                depth,
                duration: _duration,
                ..
            } => {
                let args = open_tool_args
                    .remove(&call_id)
                    .unwrap_or(serde_json::Value::Null);
                tool_echoes.push(ToolEcho {
                    result: result.clone(),
                    depth,
                    errored: error.is_some(),
                });
                let output = error.as_deref().unwrap_or(&result);
                let failed = error.is_some();
                if tools.tool_max_lines == 0 {
                    // One-line summary: `glyph tool ╱ detail`
                    let glyph = if failed { cross(color) } else { check(color) };
                    let detail = output
                        .trim()
                        .lines()
                        .find(|l| !l.trim().is_empty())
                        .unwrap_or("");
                    let detail = if detail.chars().count() > 72 {
                        let keep: String = detail.chars().take(71).collect();
                        format!("{keep}\u{2026}")
                    } else {
                        detail.to_owned()
                    };
                    if detail.is_empty() {
                        eprintln!(
                            "  {glyph} {}{}",
                            call_anchor(&name, &args),
                            depth_tag(depth)
                        );
                    } else {
                        eprintln!(
                            "  {glyph} {} \u{2571} {}{}",
                            call_anchor(&name, &args),
                            detail,
                            depth_tag(depth)
                        );
                    }
                } else {
                    eprintln!(
                        "{}",
                        render_tool_panel(&name, &args, output, failed, verbose, color)
                    );
                }
            }
            RlmEvent::AnswerSubmitted { .. } => {
                eprintln!("{} answer submitted", check(color))
            }
            RlmEvent::RunStart { .. }
            | RlmEvent::RunEnd { .. }
            | RlmEvent::IterationEnd { .. }
            | RlmEvent::RootDelta { .. }
            | RlmEvent::RootThinkingDelta { .. }
            | RlmEvent::RootText { .. }
            | RlmEvent::LimitHit { .. } => {}
        }
    }
}

/// NDJSON event printer: emits each event as a line-delimited JSON object on
/// stdout for machine-readable consumption.
pub(crate) async fn print_events_ndjson(mut rx: tokio::sync::mpsc::UnboundedReceiver<RlmEvent>) {
    while let Some(event) = rx.recv().await {
        if let Ok(line) = serde_json::to_string(&event) {
            println!("{line}");
        }
    }
}
