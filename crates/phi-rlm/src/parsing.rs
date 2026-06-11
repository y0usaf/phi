//! Ports of `rlm/utils/parsing.py`: ```repl``` block extraction and
//! iteration formatting (the REPL output fed back as the next user message).

use std::sync::LazyLock;

use phi_repl::ReplResult;
use regex_lite::Regex;

static CODE_BLOCK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)```repl[ \t]*\r?\n(.*?)\r?\n[ \t]*```[ \t]*").expect("code block regex")
});

pub fn find_code_blocks(text: &str) -> Vec<String> {
    CODE_BLOCK
        .captures_iter(text)
        .map(|c| c[1].trim().to_owned())
        .collect()
}

/// Splits `text` at the first complete ```repl fence: (prose before it,
/// remainder after it). `None` when no complete fence is present yet.
pub fn split_at_first_block(text: &str) -> Option<(&str, &str)> {
    let m = CODE_BLOCK.find(text)?;
    Some((&text[..m.start()], &text[m.end()..]))
}

/// Byte offset just past the closing fence of the last complete ```repl
/// block, or `None` when the text has no complete block.
pub fn last_block_end(text: &str) -> Option<usize> {
    CODE_BLOCK.find_iter(text).last().map(|m| m.end())
}

/// Whether `tail` (text already stripped of leading whitespace) could still
/// grow into another ```repl block: either a prefix of the opening fence or
/// an opened-but-unclosed block. Anything else is trailing prose.
pub fn could_open_block(tail: &str) -> bool {
    const OPEN: &str = "```repl";
    if tail.len() < OPEN.len() {
        return OPEN.starts_with(tail);
    }
    let Some(rest) = tail.strip_prefix(OPEN) else {
        return false;
    };
    // The fence header allows only spaces/tabs before its newline; after the
    // newline we are inside the block, where any content is fine.
    for c in rest.chars() {
        match c {
            ' ' | '\t' => continue,
            '\n' => return true,
            _ => return false,
        }
    }
    true
}

/// The prose segments surrounding the ```repl fences: always one more
/// segment than there are fences (segments may be empty).
pub fn split_prose(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut last = 0;
    for m in CODE_BLOCK.find_iter(text) {
        out.push(&text[last..m.start()]);
        last = m.end();
    }
    out.push(&text[last..]);
    out
}

#[derive(Clone, Debug)]
pub struct ExecutedBlock {
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
    out.push_str(&format!("... + [{} chars...]", total - max_chars));
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
    fn finds_repl_blocks() {
        let text = "plan\n```repl\nprint(1)\n```\nmore\n```repl\nx = 2\nprint(x)\n```\n";
        assert_eq!(find_code_blocks(text), vec!["print(1)", "x = 2\nprint(x)"]);
        assert!(find_code_blocks("```python\nprint(1)\n```").is_empty());
        assert_eq!(
            find_code_blocks("```repl\r\nprint(1)\r\n```   "),
            vec!["print(1)"]
        );
        assert_eq!(
            find_code_blocks("```repl\nprint(1)\n   ```\t"),
            vec!["print(1)"]
        );
        assert!(find_code_blocks("no code").is_empty());
    }

    #[test]
    fn splits_at_first_block() {
        let text = "plan\n```repl\nprint(1)\n```\nmore";
        assert_eq!(split_at_first_block(text), Some(("plan\n", "\nmore")));
        assert_eq!(
            split_at_first_block("plan\r\n```repl\r\nprint(1)\r\n```   \r\nmore"),
            Some(("plan\r\n", "\r\nmore"))
        );
        assert!(split_at_first_block("plan\n```repl\nprint(1)").is_none());
        assert!(split_at_first_block("no code").is_none());
    }

    #[test]
    fn finds_last_block_end() {
        let text = "plan\n```repl\nprint(1)\n```\ntail";
        let end = last_block_end(text).unwrap();
        assert_eq!(&text[end..], "\ntail");
        assert!(last_block_end("no code").is_none());
        assert!(last_block_end("```repl\nprint(1)").is_none());
    }

    #[test]
    fn detects_potential_block_openings() {
        for tail in [
            "`",
            "``",
            "```",
            "```r",
            "```repl",
            "```repl \t",
            "```repl\nx = 1",
        ] {
            assert!(could_open_block(tail), "{tail:?}");
        }
        for tail in ["p", "```python", "```replx", "```repl x", "done."] {
            assert!(!could_open_block(tail), "{tail:?}");
        }
    }

    #[test]
    fn splits_prose_around_blocks() {
        let text = "plan\n```repl\nprint(1)\n```\nmore\n```repl\nx = 2\n```\ntail";
        assert_eq!(split_prose(text), vec!["plan\n", "\nmore\n", "\ntail"]);
        assert_eq!(
            split_prose("a\r\n```repl\r\nx\r\n```   \r\nb"),
            vec!["a\r\n", "\r\nb"]
        );
        assert_eq!(split_prose("no code"), vec!["no code"]);
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
            code: "print(1)".into(),
            result: result("1\n", "", &[]),
        }];
        assert_eq!(
            format_iteration(&blocks, 20_000).unwrap(),
            "REPL output:\n\n1\n"
        );

        let blocks = vec![
            ExecutedBlock {
                code: "print(1)".into(),
                result: result("1\n", "", &[]),
            },
            ExecutedBlock {
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
            code: "print('a' * 100)".into(),
            result: result(&"a".repeat(100), "", &[]),
        }];
        let formatted = format_iteration(&blocks, 50).unwrap();
        assert!(formatted.contains("... + ["), "{formatted}");
        assert!(formatted.contains(" chars...]"), "{formatted}");
    }
}
