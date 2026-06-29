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
    assert!(
        prompt
            .contains("Custom tools and data available in the REPL:\n- bash(cmd): run a command")
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
        prompt
            .contains("Custom tools and data available in the REPL:\n- bash(cmd): run a command")
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
    assert!(prompt.contains("Run cargo tests"));
    assert!(prompt.contains("skills/cargo-test/SKILL.md"));
    assert!(prompt.contains("</available_skills>"));
    assert!(prompt.contains("The following skills provide specialized instructions"));
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

#[test]
fn identity_neutral_cyber_risk_default_on() {
    // A bare-RLM build still emits the cyber_risk guardrail via const fallback.
    let prompt = build_system_prompt(&SystemPromptOptions::default());
    assert!(prompt.contains("Assist with authorized security testing"));
    assert!(prompt.contains("NEVER generate or guess URLs"));
}

#[test]
fn cyber_risk_section_sits_between_identity_and_core_tools() {
    let prompt = build_system_prompt(&SystemPromptOptions::default());
    let identity = prompt.find("You are a Recursive Language Model").unwrap();
    let cyber = prompt
        .find("Assist with authorized security testing")
        .unwrap();
    let core = prompt.find("Available in the REPL:").unwrap();
    assert!(identity < cyber, "cyber_risk must follow identity");
    assert!(cyber < core, "cyber_risk must precede core_tools");
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
