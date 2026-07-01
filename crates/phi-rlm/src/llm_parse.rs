//! Iteration formatting: the REPL output fed back as the next user message.

use phi_repl::ReplResult;

#[derive(Clone, Debug)]
pub struct ExecutedBlock {
    /// The tool call id assigned by the LLM provider; used to match this
    /// block back to the corresponding `ToolCall` in `ChatMessage::tool_result`
    /// without relying on code-string equality (which breaks for duplicate code).
    pub id: String,
    pub code: String,
    pub result: ReplResult,
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
        let blocks = vec![ExecutedBlock {
            id: "tc_0".into(),
            code: "print(1)".into(),
            result: result("1\n", "", &[]),
        }];
        assert_eq!(
            format_iteration(&blocks, 20_000).unwrap(),
            "REPL output:\n\n1\n"
        );

        let blocks = vec![
            ExecutedBlock {
                id: "tc_0".into(),
                code: "print(1)".into(),
                result: result("1\n", "", &[]),
            },
            ExecutedBlock {
                id: "tc_1".into(),
                code: "print(2)".into(),
                result: result("2\n", "", &[]),
            },
        ];
        let formatted = format_iteration(&blocks, 20_000).unwrap();
        assert!(formatted.starts_with("REPL output (block 1):\n"));
        assert!(formatted.contains("\n\nREPL output (block 2):\n"));
        assert!(format_iteration(&[], 20_000).is_none());
    }

    #[test]
    fn truncates_long_results() {
        let blocks = vec![ExecutedBlock {
            id: "tc_0".into(),
            code: "print('a' * 100)".into(),
            result: result(&"a".repeat(100), "", &[]),
        }];
        let formatted = format_iteration(&blocks, 50).unwrap();
        assert!(formatted.contains("... + ["), "{formatted}");
        assert!(formatted.contains(" chars...]"), "{formatted}");
    }
}
