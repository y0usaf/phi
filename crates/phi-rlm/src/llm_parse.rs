//! Iteration formatting: the REPL output fed back as the next user message.

use phi_repl::ReplResult;

#[derive(Clone, Debug)]
pub struct ExecutedBlock {
    /// The tool call id assigned by the LLM provider; used to match this
    /// block back to the corresponding `ToolCall` in `ChatMessage::tool_result`
    /// without relying on argument equality (which breaks for duplicate calls).
    pub id: String,
    /// The tool that executed: `repl` or a native-exposed registry tool.
    pub name: String,
    /// The raw tool-call arguments (for `repl`, `{"code": ...}`).
    pub args: serde_json::Value,
    pub result: ReplResult,
}

impl ExecutedBlock {
    /// Convenience constructor for a `repl` execution.
    pub fn repl(id: impl Into<String>, code: &str, result: ReplResult) -> Self {
        Self {
            id: id.into(),
            name: "repl".to_owned(),
            args: serde_json::json!({ "code": code }),
            result,
        }
    }

    /// Human-readable source of this block: the Python code for `repl`
    /// executions, `name(args)` for native tool calls.
    pub fn source(&self) -> String {
        if self.name == "repl"
            && let Some(code) = self.args.get("code").and_then(|v| v.as_str())
        {
            return code.to_owned();
        }
        format!("{}({})", self.name, self.args)
    }
}

fn py_list_repr(names: &[String]) -> String {
    let quoted: Vec<String> = names.iter().map(|n| format!("'{n}'")).collect();
    format!("[{}]", quoted.join(", "))
}

pub fn format_execution_result(result: &ReplResult) -> String {
    let mut parts = Vec::new();
    if !result.stdout.is_empty() {
        parts.push(format!("\n{}", result.stdout));
    }
    if !result.stderr.is_empty() {
        parts.push(format!("\n{}", result.stderr));
    }
    if !result.vars.is_empty() {
        parts.push(format!("REPL variables: {}\n", py_list_repr(&result.vars)));
    }
    if parts.is_empty() {
        "No output".to_owned()
    } else {
        parts.join("\n\n")
    }
}

/// Aggregate per-iteration output budget, as a multiple of the per-result
/// cap (`max_output_chars`): one iteration can carry several `repl` tool
/// calls, each individually capped by the driver, so without an aggregate
/// cap a wide batch could inject N×20K chars into history in one turn.
/// (Reference: claude-code `toolLimits` keeps the same ~4-5× ratio between
/// its per-result and aggregate-per-message caps.)
pub const AGGREGATE_OUTPUT_FACTOR: usize = 5;

/// Charge `output` against the iteration's remaining aggregate budget,
/// truncating once it runs out. Results after exhaustion are elided to a
/// one-line marker so the model knows output was dropped (never silently).
pub fn clamp_aggregate(output: String, remaining: &mut usize) -> String {
    let total = output.chars().count();
    if total <= *remaining {
        *remaining -= total;
        return output;
    }
    let kept = *remaining;
    *remaining = 0;
    if kept == 0 {
        return "[output elided: aggregate per-iteration output budget exhausted — re-run this step alone or print less]".to_owned();
    }
    let mut out: String = output.chars().take(kept).collect();
    use std::fmt::Write;
    let _ = write!(
        out,
        "... + [{} chars dropped: aggregate per-iteration output budget exhausted]",
        total - kept
    );
    out
}

fn truncate_chars(text: String, max_chars: usize) -> String {
    let total = text.chars().count();
    if total <= max_chars {
        return text;
    }
    let mut out: String = text.chars().take(max_chars).collect();
    use std::fmt::Write;
    let _ = write!(out, "... + [{} chars...]", total - max_chars);
    out
}

/// The combined user message carrying all block outputs of one iteration, or
/// `None` when no code was run. The assistant message itself (the model's
/// response with the embedded code) is appended by the engine.
pub fn format_iteration(blocks: &[ExecutedBlock], max_chars: usize) -> Option<String> {
    if blocks.is_empty() {
        return None;
    }
    let multi = blocks.len() > 1;
    let parts: Vec<String> = blocks
        .iter()
        .enumerate()
        .map(|(i, block)| {
            let result = truncate_chars(format_execution_result(&block.result), max_chars);
            let header = if multi {
                format!("REPL output (block {}):", i + 1)
            } else {
                "REPL output:".to_owned()
            };
            format!("{header}\n{result}")
        })
        .collect();
    Some(parts.join("\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn result(stdout: &str, stderr: &str, vars: &[&str]) -> ReplResult {
        ReplResult {
            stdout: stdout.into(),
            stderr: stderr.into(),
            vars: vars.iter().map(|s| (*s).to_owned()).collect(),
            final_answer: None,
            duration: Duration::ZERO,
            truncated: false,
        }
    }

    #[test]
    fn formats_execution_result() {
        assert_eq!(
            format_execution_result(&result("out\n", "", &["x"])),
            "\nout\n\n\nREPL variables: ['x']\n"
        );
        assert_eq!(format_execution_result(&result("", "", &[])), "No output");
    }

    #[test]
    fn formats_single_and_multi_block_iterations() {
        let blocks = vec![ExecutedBlock::repl(
            "tc_0",
            "print(1)",
            result("1\n", "", &[]),
        )];
        assert_eq!(
            format_iteration(&blocks, 20_000).unwrap(),
            "REPL output:\n\n1\n"
        );

        let blocks = vec![
            ExecutedBlock::repl("tc_0", "print(1)", result("1\n", "", &[])),
            ExecutedBlock::repl("tc_1", "print(2)", result("2\n", "", &[])),
        ];
        let formatted = format_iteration(&blocks, 20_000).unwrap();
        assert!(formatted.starts_with("REPL output (block 1):\n"));
        assert!(formatted.contains("\n\nREPL output (block 2):\n"));
        assert!(format_iteration(&[], 20_000).is_none());
    }

    #[test]
    fn clamp_aggregate_charges_and_truncates() {
        let mut remaining = 10usize;
        // Fits: charged in full.
        assert_eq!(clamp_aggregate("abcde".to_owned(), &mut remaining), "abcde");
        assert_eq!(remaining, 5);
        // Overflows: truncated to what's left, budget zeroed.
        let out = clamp_aggregate("0123456789".to_owned(), &mut remaining);
        assert!(out.starts_with("01234"), "{out}");
        assert!(out.contains("chars dropped"), "{out}");
        assert_eq!(remaining, 0);
        // Exhausted: elided to the marker only.
        let out = clamp_aggregate("more output".to_owned(), &mut remaining);
        assert!(out.starts_with("[output elided"), "{out}");
    }

    #[test]
    fn truncates_long_results() {
        let blocks = vec![ExecutedBlock::repl(
            "tc_0",
            "print('a' * 100)",
            result(&"a".repeat(100), "", &[]),
        )];
        let formatted = format_iteration(&blocks, 50).unwrap();
        assert!(formatted.contains("... + ["), "{formatted}");
        assert!(formatted.contains(" chars...]"), "{formatted}");
    }
}
