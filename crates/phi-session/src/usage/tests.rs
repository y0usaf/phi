use super::loading::usage_entries_from_json;
use super::rendering::display_cell_width;
use super::rendering::format_model_name;
use super::*;
use serde_json::Value;
use std::fs;

fn message(timestamp: &str, model: &str, input: u64, output: u64, cost: f64) -> String {
    format!(
        r#"{{"type":"message","id":"x","parent_id":null,"timestamp":"{timestamp}","role":"assistant","text":"hi","usage":{{"input":{input},"output":{output},"cache_read":10,"cache_write":20,"cost_total":{cost},"model":"{model}"}}}}"#
    )
}

#[test]
fn usage_entry_parses_phi_session_shape() {
    let value: Value = serde_json::from_str(&message(
        "2026-01-01T00:00:01Z",
        "claude-opus-4-5",
        100,
        50,
        0.05,
    ))
    .unwrap();
    let entry = usage_entries_from_json(&value, &jiff::tz::TimeZone::UTC)
        .pop()
        .unwrap();
    assert_eq!(entry.date, "2026-01-01");
    assert_eq!(entry.model, "claude-opus-4-5");
    assert_eq!(entry.totals.input_tokens, 100);
    assert_eq!(entry.totals.cache_read_tokens, 10);
    assert_eq!(entry.totals.cache_creation_tokens, 20);
    assert_eq!(entry.totals.total_cost, 0.05);
}

#[test]
fn usage_entry_rejects_user_and_usage_less_entries() {
    let user: Value = serde_json::from_str(
            r#"{"type":"message","id":"x","timestamp":"2026-01-01T00:00:01Z","role":"user","text":"hi"}"#,
        )
        .unwrap();
    assert!(usage_entries_from_json(&user, &jiff::tz::TimeZone::UTC).is_empty());
    let no_usage: Value = serde_json::from_str(
            r#"{"type":"message","id":"x","timestamp":"2026-01-01T00:00:01Z","role":"assistant","text":"hi"}"#,
        )
        .unwrap();
    assert!(usage_entries_from_json(&no_usage, &jiff::tz::TimeZone::UTC).is_empty());
}

#[test]
fn usage_entries_expand_per_model_breakdowns() {
    let value: Value = serde_json::from_str(
            r#"{"type":"message","id":"x","timestamp":"2026-01-01T00:00:01Z","role":"assistant","text":"hi","usage":{"input":999,"output":999,"cache_read":0,"cache_write":0,"cost_total":9.99,"model":"__total__"},"usage_by_model":[{"input":100,"output":50,"cache_read":1,"cache_write":2,"cost_total":0.05,"model":"anthropic/claude-sonnet-4"},{"input":7,"output":8,"cache_read":0,"cache_write":0,"cost_total":0.01,"model":"openai/gpt-5"}]}"#,
        )
        .unwrap();

    let entries = usage_entries_from_json(&value, &jiff::tz::TimeZone::UTC);

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].model, "anthropic/claude-sonnet-4");
    assert_eq!(entries[0].totals.input_tokens, 100);
    assert_eq!(entries[1].model, "openai/gpt-5");
    assert_eq!(entries[1].totals.output_tokens, 8);
}

#[test]
fn report_keeps_same_model_same_token_calls_with_different_ids() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("--tmp-project--");
    fs::create_dir_all(&project).unwrap();
    let lines = [
            r#"{"type":"session","id":"s1","timestamp":"2026-01-01T00:00:00Z","cwd":"/tmp/project"}"#.to_owned(),
            r#"{"type":"message","id":"a","timestamp":"2026-01-03T12:00:00Z","role":"assistant","text":"hi","usage":{"input":100,"output":50,"cache_read":10,"cache_write":20,"cost_total":0.05,"model":"model-a"}}"#.to_owned(),
            r#"{"type":"message","id":"b","timestamp":"2026-01-03T12:00:00Z","role":"assistant","text":"hi","usage":{"input":100,"output":50,"cache_read":10,"cache_write":20,"cost_total":0.05,"model":"model-a"}}"#.to_owned(),
            r#"{"type":"message","id":"a","timestamp":"2026-01-03T12:00:00Z","role":"assistant","text":"hi","usage":{"input":100,"output":50,"cache_read":10,"cache_write":20,"cost_total":0.05,"model":"model-a"}}"#.to_owned(),
        ];
    fs::write(project.join("a.jsonl"), lines.join("\n")).unwrap();

    let report = load_usage_report_from_path(tmp.path());

    assert_eq!(report.entries, 2);
    assert_eq!(report.skipped_duplicates, 1);
    assert_eq!(report.totals.input_tokens, 200);
}

#[test]
fn report_dedupes_per_model_not_just_timestamp_and_tokens() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("--tmp-project--");
    fs::create_dir_all(&project).unwrap();
    let lines = [
        r#"{"type":"session","id":"s1","timestamp":"2026-01-01T00:00:00Z","cwd":"/tmp/project"}"#
            .to_owned(),
        message("2026-01-03T12:00:00Z", "model-a", 100, 50, 0.05),
        message("2026-01-03T12:00:00Z", "model-b", 100, 50, 0.07),
    ];
    fs::write(project.join("a.jsonl"), lines.join("\n")).unwrap();

    let report = load_usage_report_from_path(tmp.path());

    assert_eq!(report.entries, 2);
    assert_eq!(report.skipped_duplicates, 0);
    assert_eq!(report.totals.input_tokens, 200);
    assert_eq!(report.days[0].model_breakdowns.len(), 2);
}

#[test]
fn report_aggregates_by_day_and_model_skipping_archive_and_duplicates() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("--tmp-project--");
    fs::create_dir_all(&project).unwrap();
    let lines = [
        r#"{"type":"session","id":"s1","timestamp":"2026-01-01T00:00:00Z","cwd":"/tmp/project"}"#
            .to_owned(),
        message("2026-01-03T12:00:00Z", "claude-sonnet-4", 100, 50, 0.05),
        message("2026-01-03T12:00:00Z", "claude-sonnet-4", 100, 50, 0.05),
        message("2026-01-01T12:00:00Z", "claude-opus-4-5", 7, 8, 0.01),
    ];
    fs::write(project.join("a.jsonl"), lines.join("\n")).unwrap();
    let archive = tmp.path().join("ARCHIVE");
    fs::create_dir_all(&archive).unwrap();
    fs::write(
        archive.join("old.jsonl"),
        message("2026-01-02T12:00:00Z", "claude-opus-4-5", 999, 999, 9.0),
    )
    .unwrap();

    let report = load_usage_report_from_path(tmp.path());

    assert_eq!(report.files_scanned, 1);
    assert_eq!(report.entries, 2);
    assert_eq!(report.skipped_duplicates, 1);
    assert_eq!(report.totals.input_tokens, 107);
    assert_eq!(report.days.len(), 2);
    assert!(report.days[0].date > report.days[1].date);
    assert_eq!(
        report.days[0].model_breakdowns[0].model_name,
        "claude-sonnet-4"
    );
}

#[test]
fn formats_model_names_like_pi_usage() {
    assert_eq!(format_model_name("claude-opus-4-5"), "opus-4-5");
    assert_eq!(format_model_name("claude-sonnet-4-20250514"), "sonnet-4");
    assert_eq!(format_model_name("anthropic/claude-opus-4.5"), "opus-4.5");
}

#[test]
fn usage_rows_include_totals_and_cost() {
    let report = UsageReport {
        days: vec![UsageDay {
            date: "2026-01-02".into(),
            totals: UsageTotals {
                input_tokens: 10,
                output_tokens: 12,
                cache_creation_tokens: 1,
                cache_read_tokens: 2,
                total_cost: 0.03,
            },
            model_breakdowns: vec![
                UsageModelBreakdown {
                    model_name: "claude-sonnet-4".into(),
                    totals: UsageTotals {
                        input_tokens: 7,
                        output_tokens: 8,
                        cache_creation_tokens: 1,
                        cache_read_tokens: 2,
                        total_cost: 0.01,
                    },
                },
                UsageModelBreakdown {
                    model_name: "claude-opus-4-5".into(),
                    totals: UsageTotals {
                        input_tokens: 3,
                        output_tokens: 4,
                        cache_creation_tokens: 0,
                        cache_read_tokens: 0,
                        total_cost: 0.02,
                    },
                },
            ],
        }],
        totals: UsageTotals {
            input_tokens: 1000,
            output_tokens: 50,
            cache_creation_tokens: 20,
            cache_read_tokens: 10,
            total_cost: 0.055,
        },
        files_scanned: 1,
        entries: 1,
        skipped_duplicates: 0,
    };

    let daily_rows = usage_overlay_lines(&report);
    let header_row = usage_header_line();
    let separator_row = usage_separator_line();

    assert_eq!(
        display_cell_width(&header_row),
        display_cell_width(&separator_row)
    );
    assert!(header_row.contains("DATE / MODEL"));
    assert!(header_row.contains('│'));
    assert!(separator_row.contains('┼'));
    assert_eq!(daily_rows.len(), 3);
    assert!(daily_rows[0].contains("2026-01-02"));
    assert!(daily_rows[0].contains('│'));
    assert!(daily_rows[0].contains("25"));
    assert!(daily_rows[0].contains("$0.03"));
    assert!(daily_rows[1].contains("├─ opus-4-5"));
    assert!(daily_rows[1].contains("$0.02"));
    assert!(daily_rows[2].contains("└─ sonnet-4"));
    assert!(daily_rows[2].contains("$0.01"));

    let total_row = usage_total_line(&report.totals);

    assert!(total_row.contains("TOTAL"));
    assert!(total_row.contains("1,080"));
    assert!(total_row.contains("$0.06"));
}

#[test]
fn usage_lines_separate_day_groups() {
    let day = |date: &str| UsageDay {
        date: date.into(),
        totals: UsageTotals {
            input_tokens: 1,
            output_tokens: 2,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            total_cost: 0.01,
        },
        model_breakdowns: Vec::new(),
    };
    let report = UsageReport {
        days: vec![day("2026-01-02"), day("2026-01-01")],
        totals: UsageTotals::default(),
        files_scanned: 1,
        entries: 2,
        skipped_duplicates: 0,
    };

    let rows = usage_overlay_lines(&report);

    assert_eq!(usage_overlay_line_count(&report), 3);
    assert_eq!(rows.len(), 3);
    assert!(rows[1].starts_with('─'));
    assert!(rows[1].contains('┼'));
}
