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
    assert!(
        tool_input_lines("read_file", &serde_json::json!({"path": "src/main.rs"})).is_empty()
    );
    assert!(
        !tool_input_lines("write_file", &serde_json::json!({"path": "src/main.rs"})).is_empty()
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
fn input_output_tools_render_separators_inside_one_box() {
    let panel = render_tool_panel(
        "bash",
        &serde_json::json!({"cmd": "echo hi"}),
        "hi\n",
        false,
        true,
        false,
    );

    assert!(panel.contains("┌─── ✔ Bash"));
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
    assert!(panel.contains("├─── Input"));
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
