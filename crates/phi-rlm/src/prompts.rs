//! Pi-style composable system-prompt builder for RLM.
//!
//! The monolithic `RLM_SYSTEM_PROMPT` template has been decomposed into named,
//! overridable sections (`DEFAULT_IDENTITY`, `DEFAULT_REPL_TOOLS`,
//! `DEFAULT_MECHANICS`) assembled by `SystemPromptOptions` + `build_system_prompt`,
//! mirroring pi's `BuildSystemPromptOptions`. Custom tools, project context,
//! skills, and extension appends are now first-class composed sections rather
//! than string-replaced tokens.

use serde_json::Value;

use crate::ContextPayload;

pub const DEFAULT_IDENTITY: &str = r#"You are a Recursive Language Model (RLM): a language model with a prompt, and a very important context stored in a Python REPL related to that prompt.
You can iteratively interact with the a Python REPL, which has access to LLM calls as a function. You will be queried turn-by-turn until you have an answer to the query.

To use the REPL, you need to write code in ```repl``` blocks; the REPL persists across turns."#;

pub const DEFAULT_REPL_TOOLS: &str = r#"Available in the REPL:
- `context`: the important, potentially very long information related to the prompt (typically `str` or `list[str]`).
- `llm_query(prompt: str, model: str | None = None) -> str`: a single sub-LLM completion. Use for extraction, summarization, or Q&A over a chunk of text. Sub-LLM context window ≈ 500K chars.
- `llm_query_batched(prompts: list[str], model=None) -> list[str]`: concurrently call several LLM calls in parallel over a list of prompts; same order out as in.
- `rlm_query(prompt, model=None)` / `rlm_query_batched(prompts, model=None)`: recursive RLM sub-calls. This should be your default delegation path for non-trivial subtasks: use it when a subtask needs multi-step reasoning, its own decomposition, code/tool use, or iterative verification. Reserve `llm_query` / `llm_query_batched` for clearly one-shot extraction or summarization. Falls back to `llm_query` / `llm_query_batched` when recursion is disabled.
- `SHOW_VARS() -> str`: list every variable currently in the REPL.
- `bg(code: str) -> int`: run code as a background job sharing the REPL namespace; returns its job id immediately. `jobs() -> str` lists jobs; `job_status(id) -> str` and `job_output(id) -> str` poll one. If a ```repl``` block runs past its time limit it is NOT killed: it keeps running as a background job, and you get its output so far plus the job id to poll.
- `answer`: dict initialized to `{"content": "", "ready": False}`. To submit, set `answer["content"]` to the final answer and `answer["ready"] = True` inside a ```repl``` block."#;

pub const DEFAULT_MECHANICS: &str = r#"REPL outputs over ~20K characters are truncated, so for longer payloads slice `context` and pass slices through `llm_query` rather than `print`-ing them whole. The REPL is NOT a Jupyter cell — only `print(...)` output (stdout) is shown back to you between turns; a bare expression on the last line is silently discarded. Always wrap inspections in `print(...)`.

As a general strategy, you should start by probing your context to understand it better (e.g. print a few lines, count them, etc.). Then, use the REPL to build up an answer to the query.

Plan in prose, then write your ```repl``` block(s). To run several independent pieces of code in parallel, write multiple ```repl``` blocks back-to-back with nothing between them. React to the outputs on subsequent turns, then continue with prose and/or more blocks. Do not flip `answer["ready"] = True` on turn 1 without first inspecting `context`. A message with no ```repl``` block executes nothing: you will get one corrective reminder, and a second consecutive block-free message is accepted as your final answer as written."#;

pub const ORCHESTRATOR_ADDENDUM: &str = "As an RLM, you should act as an orchestrator, not a solver.\n\nDirectly after you probe the `context` and understand your task, pause and plan: state explicitly how the task decomposes into sub-LLM / REPL steps, and sketch the concrete sequence of turns — what each turn computes and which sub-LLM call (if any) it issues — like a condensed trajectory, before you execute them. Then execute one turn at a time: after each step `print` a small sample of the result, verify it looks right, and only flip `answer[\"ready\"] = True` once you have actually printed the candidate answer.\n\nBias strongly toward recursion. After the initial probe, if the task is not already pinned down by a direct Python search or a single short visible passage, prefer handing the substantive work to `rlm_query` / `rlm_query_batched` rather than reasoning it out yourself in the parent. Treat the parent RLM as a coordinator: plan, route, and combine; let child RLMs do the deeper solving. `llm_query` is the optimization path for obviously one-shot work, not the default for ambiguous or multi-step work. If you find yourself about to write several sentences of unaided reasoning, that is usually a sign to delegate with `rlm_query` instead. When several non-trivial subtasks are independent, prefer `rlm_query_batched` so each child can think in parallel.\n\nYour own context window is small. Push every long-context operation that would not fit comfortably in your own working window — reading, summarizing, classifying, verifying, answering sub-questions, even recapping your own progress — into `llm_query` / `llm_query_batched` calls instead of pulling that text into your own message stream. (Conversely: if a Python keyword / regex search over `context` would already pin the answer, or if a single visible passage already contains it, just read it directly — sub-LMs are for when the raw text won't fit or the question needs semantic interpretation.) Long REPL stdout pollutes history the same way raw `context` does: if you want a recap, ask `llm_query` for a 1–2 sentence summary and `print` only that. Aggregate the small results back in the REPL.\n\nSub-LLMs have no REPL; they only see the prompt and the `context` slice you pass them. Hand them clean, focused inputs and ask for terse, structured outputs you can manipulate programmatically.\n\nSub-call budget is finite on two independent axes, and `llm_query_batched` only parallelizes — it does not relax either. (1) Per-prompt capacity: a single sub-call answers well only when its input stays modestly sized — a useful rough ceiling is ~100K characters per prompt, less when the text is dense. Pack each prompt close to that capacity (a chunk of many items, a whole document) so one call accomplishes a lot of work. (2) Per-batch fan-out: `llm_query_batched` concurrency is bounded too — a useful rough ceiling is ~20 prompts per batch. Tiny-prompt mega-batches (hundreds or thousands of single-item prompts) are the anti-pattern; fat-prompt small batches are correct. For many independent units, use several ~20-wide batches of full-capacity prompts in sequence, not one mega-batch of tiny prompts. When the work can be expressed either as a sequential loop of `llm_query`s or as one comparably-sized batched call, prefer batched — same total work, far fewer turns burned. After Python-side filtering has narrowed the candidate set, batch-extract the survivors rather than reading them by hand. If the raw workload exceeds both budgets at once (e.g. a context far larger than ~20 × 100K chars), don't brute-force it: filter aggressively in Python first to a tractable subset, or stage the task — a cheap coarse pass narrows candidates, then a targeted second pass extracts from the survivors.\n\nReserve your own tokens for high-level decisions: what to ask next, how to combine sub-LM outputs, when to finalize. Delegate everything else.";

/// Appended to the system prompt only when the loop is turn-capped (depth > 0).
/// The root loop is uncapped, so priming it with turn scarcity makes it rush to
/// a final answer; capped children still benefit from the nudge.
pub const TURN_SCARCITY_NUDGE: &str = "If you are running out of turns without a confirmed answer, submit your best inference rather than letting the rollout terminate unsubmitted.";

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
    max_iterations: Option<usize>,
    context_count: usize,
    history_count: usize,
    warm: bool,
) -> String {
    let body = match max_iterations {
        Some(max_iterations) => format!("Turn {}/{}:", iteration + 1, max_iterations),
        None => format!("Turn {}:", iteration + 1),
    };
    let mut prompt = if iteration == 0 && history_count == 0 && !warm {
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
    if warm && iteration == 0 {
        prompt.push_str(
            "\n\nNote: You are continuing this session. Your previous conversation context is included above; your REPL environment and variables persist across turns. Do not re-probe `context` from scratch — build on what you already know.",
        );
    }
    prompt
}

/// A file loaded from the working directory (e.g. AGENTS.md, CLAUDE.md) to
/// inject as a `<project_context>` block in the system prompt.
#[derive(Clone, Debug, Default)]
pub struct ContextFile {
    pub name: String,
    pub content: String,
}

/// A skill discovered via SKILL.md frontmatter, injected as an
/// `<available_skills>` block in the system prompt.
#[derive(Clone, Debug, Default)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: Option<String>,
}

/// Pi-style composable system-prompt options. Each field overrides a default
/// section; `build_system_prompt` composes them in a fixed order.
#[derive(Clone, Debug, Default)]
pub struct SystemPromptOptions<'a> {
    /// Opening identity line. Defaults to `DEFAULT_IDENTITY`.
    pub identity: Option<&'a str>,
    /// Core REPL tool listing. Defaults to `DEFAULT_REPL_TOOLS`.
    pub core_tools: Option<&'a str>,
    /// Pre-formatted custom-tools listing (e.g. coding helpers). Wrapped in a
    /// "Custom tools and data available in the REPL:" header.
    pub custom_tools: Option<&'a str>,
    /// REPL mechanics / strategy / turn protocol. Defaults to
    /// `DEFAULT_MECHANICS`.
    pub mechanics: Option<&'a str>,
    /// Additional behavioral guidelines, rendered as a bulleted list.
    pub extra_guidelines: Vec<String>,
    /// Append the orchestrator addendum (act as coordinator, delegate).
    pub orchestrator: bool,
    /// Turn cap for this run: `Some(n)` for capped children, `None` for the
    /// uncapped root. Controls the turn-scarcity nudge.
    pub turn_cap: Option<usize>,
    /// Project-context files (AGENTS.md / CLAUDE.md) -> `<project_context>`.
    pub context_files: Vec<ContextFile>,
    /// Skills -> `<available_skills>`.
    pub skills: Vec<Skill>,
    /// Working directory, rendered as a trailing line.
    pub cwd: Option<&'a str>,
    /// Extension-hook append (from `filter_system_prompt`).
    pub append: Option<&'a str>,
}

/// Compose a system prompt from `SystemPromptOptions`, mirroring pi's
/// `buildSystemPrompt`. Sections are joined with blank lines in a fixed order:
///
/// identity → core_tools → custom_tools → mechanics → extra_guidelines →
/// orchestrator → turn_scarcity → project_context → available_skills →
/// cwd → append
pub fn build_system_prompt(opts: &SystemPromptOptions) -> String {
    let mut sections: Vec<String> = Vec::new();

    // identity
    sections.push(
        opts.identity
            .unwrap_or(DEFAULT_IDENTITY)
            .to_owned(),
    );

    // core_tools
    sections.push(
        opts.core_tools
            .unwrap_or(DEFAULT_REPL_TOOLS)
            .to_owned(),
    );

    // custom_tools (optional, wrapped in a header)
    if let Some(tools) = opts.custom_tools {
        if !tools.is_empty() {
            sections.push(format!(
                "Custom tools and data available in the REPL:\n{tools}"
            ));
        }
    }

    // mechanics
    sections.push(
        opts.mechanics
            .unwrap_or(DEFAULT_MECHANICS)
            .to_owned(),
    );

    // extra_guidelines (optional, bulleted)
    if !opts.extra_guidelines.is_empty() {
        let bullets: Vec<String> = opts
            .extra_guidelines
            .iter()
            .map(|g| format!("- {g}"))
            .collect();
        sections.push(format!("Guidelines:\n{}", bullets.join("\n")));
    }

    // orchestrator addendum
    if opts.orchestrator {
        sections.push(ORCHESTRATOR_ADDENDUM.to_owned());
    }

    // turn scarcity nudge (capped children only)
    if opts.turn_cap.is_some() {
        sections.push(TURN_SCARCITY_NUDGE.to_owned());
    }

    // project_context
    if !opts.context_files.is_empty() {
        let blocks: Vec<String> = opts
            .context_files
            .iter()
            .map(|f| {
                format!(
                    "<file name=\"{}\">\n{}\n</file>",
                    f.name, f.content
                )
            })
            .collect();
        sections.push(format!(
            "<project_context>\n{}\n</project_context>",
            blocks.join("\n")
        ));
    }

    // available_skills
    if !opts.skills.is_empty() {
        let entries: Vec<String> = opts
            .skills
            .iter()
            .map(|s| {
                let path_attr = s
                    .path
                    .as_deref()
                    .map(|p| format!(" path=\"{}\"", p))
                    .unwrap_or_default();
                format!(
                    "<skill name=\"{}\" description=\"{}\"{} />",
                    s.name, s.description, path_attr
                )
            })
            .collect();
        sections.push(format!(
            "<available_skills>\n{}\n</available_skills>",
            entries.join("\n")
        ));
    }

    // cwd
    if let Some(cwd) = opts.cwd {
        sections.push(format!("Working directory: {cwd}"));
    }

    // append (extension hook)
    if let Some(append) = opts.append {
        if !append.is_empty() {
            sections.push(append.to_owned());
        }
    }

    sections.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn system_prompt_default_has_no_tools_token() {
        let opts = SystemPromptOptions::default();
        let prompt = build_system_prompt(&opts);
        assert!(!prompt.contains("{custom_tools_section}"));
        assert!(prompt.contains("You are a Recursive Language Model (RLM)"));
        assert!(prompt.contains("Available in the REPL:"));
        assert!(prompt.contains("REPL outputs over ~20K"));
    }

    #[test]
    fn system_prompt_appends_orchestrator_addendum() {
        let opts = SystemPromptOptions {
            orchestrator: true,
            ..Default::default()
        };
        let prompt = build_system_prompt(&opts);
        assert!(prompt.contains("As an RLM, you should act as an orchestrator, not a solver."));
    }

    #[test]
    fn uncapped_root_omits_turn_scarcity_nudge() {
        let opts = SystemPromptOptions {
            orchestrator: true,
            turn_cap: None,
            ..Default::default()
        };
        let prompt = build_system_prompt(&opts);
        assert!(!prompt.contains("running out of turns"));
    }

    #[test]
    fn capped_child_appends_turn_scarcity_nudge() {
        let opts = SystemPromptOptions {
            orchestrator: true,
            turn_cap: Some(30),
            ..Default::default()
        };
        let prompt = build_system_prompt(&opts);
        assert!(prompt.contains("running out of turns"));
        assert!(prompt.contains(TURN_SCARCITY_NUDGE));
        assert!(!ORCHESTRATOR_ADDENDUM.contains("running out of turns"));
    }

    #[test]
    fn system_prompt_includes_custom_tools() {
        let opts = SystemPromptOptions {
            custom_tools: Some("- bash(cmd): run a command"),
            ..Default::default()
        };
        let prompt = build_system_prompt(&opts);
        assert!(prompt.contains("Custom tools and data available in the REPL:\n- bash(cmd): run a command"));
    }

    #[test]
    fn system_prompt_custom_identity_does_not_drop_tools() {
        // Regression: previously custom_system_prompt replaced the entire
        // template, silently dropping the tools listing.
        let opts = SystemPromptOptions {
            identity: Some("You are a coding agent in /home/user/project."),
            custom_tools: Some("- bash(cmd): run a command"),
            ..Default::default()
        };
        let prompt = build_system_prompt(&opts);
        assert!(prompt.contains("You are a coding agent in /home/user/project."));
        assert!(prompt.contains("Custom tools and data available in the REPL:\n- bash(cmd): run a command"));
        // Core REPL tools should still be present even with custom identity.
        assert!(prompt.contains("Available in the REPL:"));
        assert!(prompt.contains("llm_query"));
    }

    #[test]
    fn system_prompt_includes_project_context() {
        let opts = SystemPromptOptions {
            context_files: vec![ContextFile {
                name: "AGENTS.md".to_owned(),
                content: "Use Rust edition 2021.".to_owned(),
            }],
            ..Default::default()
        };
        let prompt = build_system_prompt(&opts);
        assert!(prompt.contains("<project_context>"));
        assert!(prompt.contains("<file name=\"AGENTS.md\">"));
        assert!(prompt.contains("Use Rust edition 2021."));
        assert!(prompt.contains("</project_context>"));
    }

    #[test]
    fn system_prompt_includes_skills() {
        let opts = SystemPromptOptions {
            skills: vec![Skill {
                name: "cargo-test".to_owned(),
                description: "Run cargo tests".to_owned(),
                path: Some("skills/cargo-test/SKILL.md".to_owned()),
            }],
            ..Default::default()
        };
        let prompt = build_system_prompt(&opts);
        assert!(prompt.contains("<available_skills>"));
        assert!(prompt.contains("cargo-test"));
        assert!(prompt.contains("</available_skills>"));
    }

    #[test]
    fn system_prompt_includes_cwd() {
        let opts = SystemPromptOptions {
            cwd: Some("/home/user/project"),
            ..Default::default()
        };
        let prompt = build_system_prompt(&opts);
        assert!(prompt.contains("Working directory: /home/user/project"));
    }

    #[test]
    fn system_prompt_includes_append() {
        let opts = SystemPromptOptions {
            append: Some("Extra extension instructions."),
            ..Default::default()
        };
        let prompt = build_system_prompt(&opts);
        assert!(prompt.contains("Extra extension instructions."));
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
            build_user_prompt(0, Some(30), 1, 0, false),
            "You have not interacted with the REPL environment or seen your prompt / context yet. Look at the context first; do not provide a final answer yet.\n\nTurn 1/30:"
        );
        assert_eq!(build_user_prompt(4, Some(30), 1, 0, false), "Turn 5/30:");
        assert_eq!(build_user_prompt(4, None, 1, 0, false), "Turn 5:");
        // Warm session: no cold-start preamble, continuation note on turn 1.
        assert_eq!(
            build_user_prompt(0, Some(30), 1, 2, true),
            "Turn 1/30:\n\nNote: You have 2 prior conversation histories available (history_0 through history_1).\n\nNote: You are continuing this session. Your previous conversation context is included above; your REPL environment and variables persist across turns. Do not re-probe `context` from scratch — build on what you already know."
        );
        // Warm session, turn 2+: no continuation note (only on turn 1).
        assert_eq!(
            build_user_prompt(1, Some(10), 2, 1, true),
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
