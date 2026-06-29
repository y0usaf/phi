use crate::format::{format_model_pricing, format_usage_summary};
use crate::render::*;
use crate::tool_view::*;
use phi_ai::{Api, Cost, CostRates, Model, Usage};
use std::collections::BTreeMap;

fn test_model() -> Model {
    Model {
        id: "m".into(),
        name: "Model".into(),
        api: Api::AnthropicMessages,
        provider: "anthropic".into(),
        base_url: "https://example.test".into(),
        reasoning: false,
        input: vec![],
        cost: CostRates {
            input: 3.0,
            output: 15.0,
            cache_read: 0.3,
            cache_write: 3.75,
        },
        context_window: 200_000,
        max_tokens: 8_192,
        headers: BTreeMap::new(),
        thinking_levels: None,
    }
}

#[test]
fn format_model_pricing_includes_cache_rates_when_present() {
    assert_eq!(
        format_model_pricing(&test_model()),
        "in $3.00/M · out $15/M · cache+ $3.75/M · cache↺ $0.3000/M"
    );
}

#[test]
fn format_usage_summary_shows_breakdown_and_total() {
    let usage = Usage {
        input: 1_200,
        output: 34_000,
        cache_read: 5_600,
        cache_write: 7_800,
        total_tokens: 48_600,
        cost: Cost {
            total: 0.1234,
            ..Cost::default()
        },
    };
    assert_eq!(
        format_usage_summary(&usage),
        "usage: in 1.2k · out 34k · cache+ 7.8k · cache↺ 5.6k · total 48k · $0.1234"
    );
}

#[test]
fn observational_tools_hide_args_and_prettify_names() {
    assert_eq!(tool_display_name("read_file"), "Read File");
    assert!(!tool_shows_args_section("read_file"));
    assert!(tool_input_lines("read_file", &serde_json::json!({"path": "src/main.rs"})).is_empty());
    // write_file's path is the canonical key (shown in the title suffix), so the
    // Args section strips it and keeps only the residual (content).
    let write_args = tool_input_lines(
        "write_file",
        &serde_json::json!({"path": "src/main.rs", "content": "fn main() {}\n"}),
    );
    let joined = write_args.join("\n");
    assert!(!joined.is_empty(), "write_file should keep residual args");
    assert!(
        joined.contains("content"),
        "residual should keep content: {joined}"
    );
    assert!(
        !joined.contains("src/main.rs"),
        "residual should drop the path already in the title: {joined}"
    );
    assert_eq!(
        tool_title_suffix("read_file", &serde_json::json!({"path": "src/main.rs"})),
        Some(" — src/main.rs".to_owned())
    );
    assert!(
        tool_shows_args_section("edit_file"),
        "edit_file should render args section"
    );
}

#[test]
fn call_anchor_shows_identity_once() {
    // Path-bearing tools: anchor carries the path (no key=value dump).
    assert_eq!(
        call_anchor("read_file", &serde_json::json!({"path": "src/main.rs"})),
        "Read File — src/main.rs"
    );
    assert_eq!(
        call_anchor(
            "edit_file",
            &serde_json::json!({"path": "a.rs", "content": "x"})
        ),
        "Edit File — a.rs"
    );
    // shell: first command line inlined; multi-line truncated.
    assert_eq!(
        call_anchor("shell", &serde_json::json!({"cmd": "echo hi"})),
        "Shell: $ echo hi"
    );
    assert!(
        call_anchor("shell", &serde_json::json!({"cmd": "echo one\necho two"})).ends_with('…'),
        "multi-line shell anchor truncates"
    );
    // grep: pattern (and path) inlined.
    assert_eq!(
        call_anchor(
            "grep",
            &serde_json::json!({"pattern": "foo", "path": "src"})
        ),
        "Grep — foo in src"
    );
}

#[test]
fn residual_args_strips_canonical_key_only() {
    // edit_file: path stripped, content/old_string kept.
    let r = residual_args(
        "edit_file",
        &serde_json::json!({"path": "a.rs", "old_string": "x", "new_string": "y"}),
    );
    let map = r.as_object().unwrap();
    assert!(!map.contains_key("path"));
    assert!(map.contains_key("old_string") && map.contains_key("new_string"));
    // unknown tool: nothing stripped (no canonical key in any title).
    let u = residual_args("custom_tool", &serde_json::json!({"a": 1, "b": 2}));
    assert_eq!(u.as_object().unwrap().len(), 2);
    // edit_file with only a path -> empty residual (no Args section).
    assert!(
        residual_args("edit_file", &serde_json::json!({"path": "a.rs"}))
            .as_object()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn input_output_tools_render_separators_inside_one_box() {
    let panel = render_tool_panel(
        "shell",
        &serde_json::json!({"cmd": "echo hi"}),
        "hi\n",
        false,
        true,
        false,
    );

    assert!(panel.contains("┌─── ✔ Shell"));
    assert!(panel.contains("├─── Command"));
    assert!(panel.contains("│ $ echo hi"));
    assert!(panel.contains("├─── Output"));
    assert!(panel.contains("│ hi"));
    assert_eq!(panel.matches('┌').count(), 1);
    assert_eq!(panel.matches('└').count(), 1);
}

#[test]
fn repl_renders_input_output_sections() {
    let panel = render_tool_panel(
        "repl",
        &serde_json::json!({"code": "print(1)"}),
        "1\n",
        false,
        true,
        false,
    );

    assert!(panel.contains("┌─── ✔ Repl"));
    // Input label removed for minimalist TUI
    assert!(panel.contains("│ print(1)"));
    assert!(panel.contains("├─── Output"));
    assert!(panel.contains("│ 1"));
}

#[test]
fn observational_tools_render_as_single_output_box() {
    let panel = render_tool_panel(
        "read_file",
        &serde_json::json!({"path": "src/main.rs"}),
        "fn main() {}\n",
        false,
        true,
        false,
    );

    assert!(panel.contains("┌─── ✔ Read File — src/main.rs"));
    assert!(!panel.contains("├─── Args"));
    assert!(!panel.contains("├─── Output"));
    assert!(panel.contains("│ fn main() {}"));
    assert_eq!(panel.matches('┌').count(), 1);
    assert_eq!(panel.matches('└').count(), 1);
}

#[test]
fn shell_body_preserved_multiline() {
    // The Command section preserves every line of a multi-line shell cmd (one
    // `$ <line>` row each), not just the first line. The panel title carries no
    // command text (the shell title suffix is empty); the full command is shown
    // only here, once, not truncated. `call_anchor` separately truncates the
    // start line — tested in `call_anchor_shows_identity_once`.
    let panel = render_tool_panel(
        "shell",
        &serde_json::json!({"cmd": "cargo test\ncargo build --release"}),
        "running 12 tests\n",
        false,
        true,
        false,
    );

    assert!(panel.contains("┌─── ✔ Shell"));
    assert!(panel.contains("├─── Command"));
    assert!(panel.contains("│ $ cargo test"));
    assert!(panel.contains("│ $ cargo build --release"));
    // No Args section re-stating the command (anchor-once, data-level).
    assert!(!panel.contains("├─── Args"));
    // Output section still renders, separate from the command body.
    assert!(panel.contains("├─── Output"));
    assert!(panel.contains("│ running 12 tests"));
    assert_eq!(panel.matches('┌').count(), 1);
    assert_eq!(panel.matches('└').count(), 1);
}
