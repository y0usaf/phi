use std::collections::HashMap;
use std::io::IsTerminal;

use phi_rlm::RlmEvent;
use phi_session::config::ToolsSettings;
use phi_session::tool_summary::{ToolCategory, categorize};

use crate::render::render_tool_panel;
use crate::tool_view::{
    ToolEcho, call_anchor, check, cross, depth_tag, indent, preview, spin, strip_tool_echoes,
};

/// Accumulated tool info for summary rendering within a REPL block.
struct PendingTool {
    name: String,
    failed: bool,
}

pub(crate) async fn print_events(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<RlmEvent>,
    verbose: bool,
    max_iterations: usize,
    tools: ToolsSettings,
) {
    let color = std::io::stderr().is_terminal();
    let mut open_tool_args: HashMap<u64, serde_json::Value> = HashMap::new();
    let mut tool_echoes: Vec<ToolEcho> = Vec::new();
    let mut pending_tools: Vec<PendingTool> = Vec::new();
    let mut repl_count: usize = 0;
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
                if tools.tool_summary {
                    repl_count += 1;
                } else {
                    eprintln!("{} exec", spin(color));
                    if tools.repl_max_lines > 0 {
                        eprintln!("{}", indent(&preview(&code, tools.repl_max_lines, verbose)));
                    }
                }
            }
            RlmEvent::CodeExecEnd {
                depth,
                stdout,
                stderr,
                duration,
                ..
            } => {
                if tools.tool_summary {
                    // When tool_summary is enabled, REPL block output is
                    // suppressed; tool calls are accumulated across all blocks
                    // and printed as a single consolidated summary at turn end.
                    let _ = (depth, stdout, stderr, duration);
                } else {
                    let glyph = if stderr.trim().is_empty() {
                        check(color)
                    } else {
                        cross(color)
                    };
                    if (duration.as_secs_f64() * 10.0).round() <= 0.0 {
                        eprintln!("  {glyph} exec");
                    } else {
                        eprintln!("  {glyph} exec \u{2022} {:.1}s", duration.as_secs_f64());
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
                if !tools.tool_summary {
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
                let failed = error.is_some();
                if tools.tool_summary {
                    pending_tools.push(PendingTool {
                        name: name.clone(),
                        failed,
                    });
                } else {
                    let output = error.as_deref().unwrap_or(&result);
                    if tools.tool_max_lines == 0 {
                        // One-line summary: `glyph tool \u{2571} detail`
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
            }
            RlmEvent::Message { text, .. } => {
                eprintln!("{text}");
            }
            RlmEvent::AnswerSubmitted { .. } => {
                if tools.tool_summary && (!pending_tools.is_empty() || repl_count > 0) {
                    print_tool_summary(&pending_tools, repl_count, color);
                    pending_tools.clear();
                    repl_count = 0;
                }
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

    // Flush any remaining accumulated tool summary at stream close
    // (covers cases where the model hits max iterations without
    // submitting an answer).
    if tools.tool_summary && (!pending_tools.is_empty() || repl_count > 0) {
        print_tool_summary(&pending_tools, repl_count, color);
    }
}

/// Print a consolidated summary of tool calls across a turn.
///
/// When `tool_summary` is enabled, tool calls from all REPL blocks in a
/// turn are accumulated and printed as a single summary at turn end.
///
/// ```text
///   \u{2713} 15 repls
///   \u{2713} 14 tools
///   \u{2713} 5 reads
///   \u{2713} 6 edits
///   \u{2713} 3 commands
/// ```
///
/// Failures are shown with \u{2717} and the count in red (when color is on):
///
/// ```text
///   \u{2713} 5 reads
///   \u{2717} 1
/// ```
fn print_tool_summary(tools: &[PendingTool], repl_count: usize, color: bool) {
    let ck = check(color);
    if repl_count > 0 {
        let noun = if repl_count == 1 { "repl" } else { "repls" };
        eprintln!("  {ck} {repl_count} {noun}");
    }
    let total = tools.len();
    if total > 0 {
        let noun = if total == 1 { "tool" } else { "tools" };
        eprintln!("    {ck} {total} {noun}");
    }

    // Build per-category tallies.
    let mut tallies: [(ToolCategory, usize, usize); 5] = [
        (ToolCategory::Reads, 0, 0),
        (ToolCategory::Edits, 0, 0),
        (ToolCategory::Commands, 0, 0),
        (ToolCategory::Searches, 0, 0),
        (ToolCategory::Other, 0, 0),
    ];
    for tool in tools {
        let cat = categorize(&tool.name);
        let idx = ToolCategory::order()
            .iter()
            .position(|c| *c == cat)
            .unwrap();
        if tool.failed {
            tallies[idx].2 += 1;
        } else {
            tallies[idx].1 += 1;
        }
    }

    let cr = cross(color);
    for (cat, ok, failed) in tallies {
        if ok == 0 && failed == 0 {
            continue;
        }
        // Pluralize: "1 read" vs "2 reads", "1 edit" vs "2 edits", etc.
        let noun = match cat {
            ToolCategory::Reads => {
                if ok == 1 {
                    "read"
                } else {
                    "reads"
                }
            }
            ToolCategory::Edits => {
                if ok == 1 {
                    "edit"
                } else {
                    "edits"
                }
            }
            ToolCategory::Commands => {
                if ok == 1 {
                    "command"
                } else {
                    "commands"
                }
            }
            ToolCategory::Searches => {
                if ok == 1 {
                    "search"
                } else {
                    "searches"
                }
            }
            ToolCategory::Other => {
                if ok == 1 {
                    "other"
                } else {
                    "others"
                }
            }
        };
        if ok > 0 {
            eprintln!("    {ck} {ok} {noun}");
        }
        if failed > 0 {
            eprintln!("    {cr} {failed}");
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
