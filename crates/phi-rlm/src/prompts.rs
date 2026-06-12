//! Ports of upstream RLM prompts (`rlm/utils/prompts.py`), with minor
//! phi-specific additions (custom tools, background jobs, and session notes).

use serde_json::Value;

use crate::ContextPayload;

pub const RLM_SYSTEM_PROMPT: &str = r#"You are a Recursive Language Model (RLM): a language model with a prompt, and a very important context stored in a Python REPL related to that prompt.
You can iteratively interact with the a Python REPL, which has access to LLM calls as a function. You will be queried turn-by-turn until you have an answer to the query.

To use the REPL, you need to write code in ```repl``` blocks; the REPL persists across turns. Available in the REPL:
- `context`: the important, potentially very long information related to the prompt (typically `str` or `list[str]`).
- `llm_query(prompt: str, model: str | None = None) -> str`: a single sub-LLM completion. Use for extraction, summarization, or Q&A over a chunk of text. Sub-LLM context window ≈ 500K chars.
- `llm_query_batched(prompts: list[str], model=None) -> list[str]`: concurrently call several LLM calls in parallel over a list of prompts; same order out as in.
- `rlm_query(prompt, model=None)` / `rlm_query_batched(prompts, model=None)`: recursive RLM sub-calls. Fall back to `llm_query` / `llm_query_batched` when recursion is disabled.
- `SHOW_VARS() -> str`: list every variable currently in the REPL.
- `bg(code: str) -> int`: run code as a background job sharing the REPL namespace; returns its job id immediately. `jobs() -> str` lists jobs; `job_status(id) -> str` and `job_output(id) -> str` poll one. If a ```repl``` block runs past its time limit it is NOT killed: it keeps running as a background job, and you get its output so far plus the job id to poll.
- `answer`: dict initialized to `{"content": "", "ready": False}`. To submit, set `answer["content"]` to the final answer and `answer["ready"] = True` inside a ```repl``` block.
{custom_tools_section}

REPL outputs over ~20K characters are truncated, so for longer payloads slice `context` and pass slices through `llm_query` rather than `print`-ing them whole. The REPL is NOT a Jupyter cell — only `print(...)` output (stdout) is shown back to you between turns; a bare expression on the last line is silently discarded. Always wrap inspections in `print(...)`.

As a general strategy, you should start by probing your context to understand it better (e.g. print a few lines, count them, etc.). Then, use the REPL to build up an answer to the query.

Plan in prose, then write your ```repl``` block(s). To run several independent pieces of code in parallel, write multiple ```repl``` blocks back-to-back with nothing between them. React to the outputs on subsequent turns, then continue with prose and/or more blocks. Do not flip `answer["ready"] = True` on turn 1 without first inspecting `context`. A message with no ```repl``` block executes nothing: you will get one corrective reminder, and a second consecutive block-free message is accepted as your final answer as written.
"#;

pub const ORCHESTRATOR_ADDENDUM: &str = "As an RLM, you should act as an orchestrator, not a solver.\n\nDirectly after you probe the `context` and understand your task, pause and plan: state explicitly how the task decomposes into sub-LLM / REPL steps, and sketch the concrete sequence of turns — what each turn computes and which sub-LLM call (if any) it issues — like a condensed trajectory, before you execute them. Then execute one turn at a time: after each step `print` a small sample of the result, verify it looks right, and only flip `answer[\"ready\"] = True` once you have actually printed the candidate answer. If you are running out of turns without a confirmed answer, submit your best inference rather than letting the rollout terminate unsubmitted.\n\nYour own context window is small. Push every long-context operation that would not fit comfortably in your own working window — reading, summarizing, classifying, verifying, answering sub-questions, even recapping your own progress — into `llm_query` / `llm_query_batched` calls instead of pulling that text into your own message stream. (Conversely: if a Python keyword / regex search over `context` would already pin the answer, or if a single visible passage already contains it, just read it directly — sub-LMs are for when the raw text won't fit or the question needs semantic interpretation.) Long REPL stdout pollutes history the same way raw `context` does: if you want a recap, ask `llm_query` for a 1–2 sentence summary and `print` only that. Aggregate the small results back in the REPL.\n\nSub-LLMs have no REPL; they only see the prompt and the `context` slice you pass them. Hand them clean, focused inputs and ask for terse, structured outputs you can manipulate programmatically.\n\nSub-call budget is finite on two independent axes, and `llm_query_batched` only parallelizes — it does not relax either. (1) Per-prompt capacity: a single sub-call answers well only when its input stays modestly sized — a useful rough ceiling is ~100K characters per prompt, less when the text is dense. Pack each prompt close to that capacity (a chunk of many items, a whole document) so one call accomplishes a lot of work. (2) Per-batch fan-out: `llm_query_batched` concurrency is bounded too — a useful rough ceiling is ~20 prompts per batch. Tiny-prompt mega-batches (hundreds or thousands of single-item prompts) are the anti-pattern; fat-prompt small batches are correct. For many independent units, use several ~20-wide batches of full-capacity prompts in sequence, not one mega-batch of tiny prompts. When the work can be expressed either as a sequential loop of `llm_query`s or as one comparably-sized batched call, prefer batched — same total work, far fewer turns burned. After Python-side filtering has narrowed the candidate set, batch-extract the survivors rather than reading them by hand. If the raw workload exceeds both budgets at once (e.g. a context far larger than ~20 × 100K chars), don't brute-force it: filter aggressively in Python first to a tractable subset, or stage the task — a cheap coarse pass narrows candidates, then a targeted second pass extracts from the survivors.\n\nReserve your own tokens for high-level decisions: what to ask next, how to combine sub-LM outputs, when to finalize. Delegate everything else.";

pub const DEFAULT_ANSWER_NUDGE: &str =
    "Please provide a final answer to the user's question based on the information provided.";

/// Corrective reminder injected after a turn with no ```repl``` block.
/// Sent as a user message because providers hoist `Role::System` messages
/// into the top-level system prompt, losing their position in the turn order.
pub const PROSE_NUDGE: &str = "<system-reminder>Your previous message contained no ```repl``` block, so nothing was executed and no answer was submitted. If it was meant as your final answer, submit it now: set answer[\"content\"] to it and answer[\"ready\"] = True in a ```repl``` block. Otherwise, continue working in ```repl``` blocks. If your next message also contains no ```repl``` block, it will be accepted as your final answer as written.</system-reminder>";

#[derive(Clone, Copy, Debug)]
pub struct QueryMetadata {
    pub context_type: &'static str,
    pub context_total_length: usize,
}

impl QueryMetadata {
    pub fn from_payload(payload: &ContextPayload) -> Self {
        fn value_len(value: &Value) -> usize {
            match value {
                Value::String(s) => s.chars().count(),
                other => other.to_string().chars().count(),
            }
        }
        match payload {
            ContextPayload::Str(s) => Self {
                context_type: "str",
                context_total_length: s.chars().count(),
            },
            ContextPayload::Json(Value::String(s)) => Self {
                context_type: "str",
                context_total_length: s.chars().count(),
            },
            ContextPayload::Json(Value::Object(map)) => Self {
                context_type: "dict",
                context_total_length: map.values().map(value_len).sum(),
            },
            ContextPayload::Json(Value::Array(items)) => {
                let total = items
                    .iter()
                    .map(|item| match item {
                        Value::Object(map) => match map.get("content") {
                            Some(content) => value_len(content),
                            None => item.to_string().chars().count(),
                        },
                        other => value_len(other),
                    })
                    .sum();
                Self {
                    context_type: "list",
                    context_total_length: total,
                }
            }
            ContextPayload::Json(other) => Self {
                context_type: "str",
                context_total_length: other.to_string().chars().count(),
            },
        }
    }
}

/// System prompt: template (default `RLM_SYSTEM_PROMPT`) with the custom-tools
/// section substituted, plus the orchestrator addendum when enabled.
pub fn build_system_prompt(
    template: Option<&str>,
    tools_formatted: Option<&str>,
    orchestrator: bool,
) -> String {
    let section = match tools_formatted {
        Some(tools) if !tools.is_empty() => {
            format!("\n6. Custom tools and data available in the REPL:\n{tools}")
        }
        _ => String::new(),
    };
    let mut prompt = template
        .unwrap_or(RLM_SYSTEM_PROMPT)
        .replace("{custom_tools_section}", &section);
    if orchestrator {
        prompt = format!("{prompt}\n\n{ORCHESTRATOR_ADDENDUM}");
    }
    prompt
}

/// The metadata user message that follows the system prompt.
pub fn metadata_user_prompt(metadata: QueryMetadata, root_prompt: Option<&str>) -> String {
    let body = format!(
        "Your context is a {} of {} total characters. Each sub-LLM call can handle roughly ~100k tokens at once.",
        metadata.context_type, metadata.context_total_length
    );
    match root_prompt {
        Some(root_prompt) => format!("Answer the following: {root_prompt}\n\n{body}"),
        None => body,
    }
}

/// Per-turn user prompt. `iteration` is 0-based.
pub fn build_user_prompt(
    iteration: usize,
    max_iterations: usize,
    context_count: usize,
    history_count: usize,
) -> String {
    let body = format!("Turn {}/{}:", iteration + 1, max_iterations);
    let mut prompt = if iteration == 0 && history_count == 0 {
        format!(
            "You have not interacted with the REPL environment or seen your prompt / context yet. Look at the context first; do not provide a final answer yet.\n\n{body}"
        )
    } else {
        body
    };
    if context_count > 1 {
        prompt.push_str(&format!(
            "\n\nNote: You have {} contexts available (context_0 through context_{}).",
            context_count,
            context_count - 1
        ));
    }
    if history_count == 1 {
        prompt.push_str(
            "\n\nNote: You have 1 prior conversation history available in the `history` variable.",
        );
    } else if history_count > 1 {
        prompt.push_str(&format!(
            "\n\nNote: You have {} prior conversation histories available (history_0 through history_{}).",
            history_count,
            history_count - 1
        ));
    }
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn system_prompt_substitutes_empty_tools_section() {
        let prompt = build_system_prompt(None, None, false);
        assert!(!prompt.contains("{custom_tools_section}"));
        assert!(prompt.contains("inside a ```repl``` block.\n\n\nREPL outputs"));
        assert!(!prompt.contains("orchestrator"));
    }

    #[test]
    fn system_prompt_appends_orchestrator_addendum() {
        let prompt = build_system_prompt(None, None, true);
        assert!(prompt.contains("\n\nAs an RLM, you should act as an orchestrator, not a solver."));
        assert!(prompt.ends_with("Delegate everything else."));
    }

    #[test]
    fn system_prompt_includes_tools_section() {
        let prompt = build_system_prompt(None, Some("- bash(cmd): run a command"), false);
        assert!(prompt.contains(
            "\n6. Custom tools and data available in the REPL:\n- bash(cmd): run a command\n"
        ));
    }

    #[test]
    fn metadata_prompt_matches_upstream() {
        let metadata = QueryMetadata::from_payload(&ContextPayload::Str("abcde".into()));
        assert_eq!(
            metadata_user_prompt(metadata, Some("What is the magic number?")),
            "Answer the following: What is the magic number?\n\nYour context is a str of 5 total characters. Each sub-LLM call can handle roughly ~100k tokens at once."
        );
        let metadata = QueryMetadata::from_payload(&ContextPayload::Json(json!(["ab", "cd"])));
        assert_eq!(
            metadata_user_prompt(metadata, None),
            "Your context is a list of 4 total characters. Each sub-LLM call can handle roughly ~100k tokens at once."
        );
    }

    #[test]
    fn user_prompt_matches_upstream() {
        assert_eq!(
            build_user_prompt(0, 30, 1, 0),
            "You have not interacted with the REPL environment or seen your prompt / context yet. Look at the context first; do not provide a final answer yet.\n\nTurn 1/30:"
        );
        assert_eq!(build_user_prompt(4, 30, 1, 0), "Turn 5/30:");
        // Warm session: no cold-start preamble even on the first turn.
        assert_eq!(
            build_user_prompt(0, 30, 1, 2),
            "Turn 1/30:\n\nNote: You have 2 prior conversation histories available (history_0 through history_1)."
        );
        assert_eq!(
            build_user_prompt(1, 10, 2, 1),
            "Turn 2/10:\n\nNote: You have 2 contexts available (context_0 through context_1).\n\nNote: You have 1 prior conversation history available in the `history` variable."
        );
    }

    #[test]
    fn query_metadata_counts_message_lists() {
        let payload = ContextPayload::Json(json!([
            {"role": "user", "content": "abc"},
            {"role": "assistant", "content": "de"}
        ]));
        let metadata = QueryMetadata::from_payload(&payload);
        assert_eq!(metadata.context_type, "list");
        assert_eq!(metadata.context_total_length, 5);
    }
}
