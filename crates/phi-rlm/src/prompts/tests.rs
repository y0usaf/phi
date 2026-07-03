use super::*;
use crate::lm_core::ContextPayload;
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
fn capped_child_appends_turn_budget_and_scarcity_nudge() {
    let opts = SystemPromptOptions {
        orchestrator: true,
        turn_cap: Some(30),
        ..Default::default()
    };
    let prompt = build_system_prompt(&opts);
    // WS9.2: the cap itself lives in the system prompt, not per-turn messages.
    assert!(prompt.contains("hard cap of 30 turns"));
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
    assert!(
        prompt.contains("Custom tools and data available in the REPL:\n- bash(cmd): run a command")
    );
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
    assert!(
        prompt.contains("Custom tools and data available in the REPL:\n- bash(cmd): run a command")
    );
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
fn session_notes_one_time_per_send() {
    // WS9.2: no per-turn "Turn N:" scaffolding exists anymore. Environment
    // notes are emitted once per send, and only when there is something to
    // note — a cold single-context run gets nothing.
    assert_eq!(session_notes(1, 0, false), None);
    // Warm continuation with prior histories.
    assert_eq!(
        session_notes(1, 2, true).as_deref(),
        Some(
            "Note: You have 2 prior conversation histories available (history_0 through history_1).\n\nNote: You are continuing this session. Your previous conversation context is included above; your REPL environment and variables persist across turns. Do not re-probe `context` from scratch — build on what you already know."
        )
    );
    // Extra contexts + a single prior history, cold restore path.
    assert_eq!(
        session_notes(2, 1, false).as_deref(),
        Some(
            "Note: You have 2 contexts available (context_0 through context_1).\n\nNote: You have 1 prior conversation history available in the `history` variable."
        )
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

#[test]
fn no_cyber_risk_section_by_default() {
    // Matches ref/pi + ref/oh-my-pi, which ship no in-prompt security or
    // URL-guessing instruction. The enumerated attack-technique list used to
    // trip newer models' dual-use input classifiers into a blanket refusal.
    let prompt = build_system_prompt(&SystemPromptOptions::default());
    assert!(!prompt.contains("Assist with authorized security testing"));
    assert!(!prompt.contains("NEVER generate or guess URLs"));
}

#[test]
fn identity_section_precedes_core_tools() {
    let prompt = build_system_prompt(&SystemPromptOptions::default());
    let identity = prompt.find("You are a Recursive Language Model").unwrap();
    let core = prompt.find("Available in the REPL:").unwrap();
    assert!(identity < core, "identity must precede core_tools");
}

#[test]
fn env_info_precedes_cwd_line() {
    let opts = SystemPromptOptions {
        env_info: Some("# Environment\n - Primary working directory: /x"),
        cwd: Some("/x"),
        ..Default::default()
    };
    let prompt = build_system_prompt(&opts);
    let env = prompt.find("# Environment").unwrap();
    let cwd = prompt.find("Working directory: /x").unwrap();
    assert!(env < cwd, "env_info block must precede the cwd line");
}
