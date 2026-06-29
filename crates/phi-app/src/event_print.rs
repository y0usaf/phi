use std::collections::HashMap;
use std::io::IsTerminal;

use phi_rlm::RlmEvent;

use crate::render::render_tool_panel;
use crate::tool_view::{
    ToolEcho, call_anchor, check, cross, depth_tag, indent, preview, spin, strip_tool_echoes,
};

pub(crate) async fn print_events(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<RlmEvent>,
    verbose: bool,
    max_iterations: usize,
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
                eprintln!("{}", indent(&preview(&code, 16, verbose)));
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
                if !stdout.trim().is_empty() {
                    eprintln!("{}", indent(&preview(&stdout, 12, verbose)));
                }
                if !stderr.trim().is_empty() {
                    eprintln!("{}", indent(&preview(&stderr, 12, verbose)));
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
                eprintln!(
                    "{}",
                    render_tool_panel(
                        &name,
                        &args,
                        error.as_deref().unwrap_or(&result),
                        error.is_some(),
                        verbose,
                        color
                    )
                );
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
