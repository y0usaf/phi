
use super::*;

fn render(
    tx: &mut Transcript,
    viewport_height: usize,
    inputs: &[(&str, Vec<String>, bool)],
) -> Frame {
    let specs: Vec<BlockSpec<'_>> = inputs
        .iter()
        .map(|(key, lines, finalized)| BlockSpec {
            key,
            lines,
            finalized: *finalized,
            max_height: None,
        })
        .collect();
    tx.render(viewport_height, &specs)
}

#[test]
fn exactly_one_blank_separator_between_visible_blocks() {
    let mut tx = Transcript::new();
    let frame = render(
        &mut tx,
        20,
        &[
            ("a", vec!["".into(), "a".into(), "".into()], true),
            ("b", vec!["b".into()], true),
        ],
    );
    assert_eq!(frame.full, vec!["a", "", "b"]);
}

#[test]
fn earliest_unfinalized_block_starts_live_region() {
    let mut tx = Transcript::new();
    let frame = render(
        &mut tx,
        20,
        &[
            ("done", vec!["done".into()], true),
            ("pending", vec!["pending".into()], false),
            ("card", vec!["card".into()], true),
        ],
    );
    assert_eq!(frame.full, vec!["done", "", "pending", "", "card"]);
    assert_eq!(frame.live_region_start, Some(2));
}

#[test]
fn non_finalized_block_does_not_enter_scrollback() {
    let mut tx = Transcript::new();
    let frame = render(
        &mut tx,
        3,
        &[
            ("history", vec!["history".into()], true),
            (
                "stream",
                vec![
                    "one".into(),
                    "two".into(),
                    "three".into(),
                    "four".into(),
                    "five".into(),
                ],
                false,
            ),
        ],
    );
    assert_eq!(frame.scrollback, vec!["history", ""]);
    assert_eq!(frame.committed_rows, 2);
    assert_eq!(frame.live, vec!["one", "two", "three", "four", "five"]);
}

#[test]
fn viewport_commits_scrolled_finalized_rows() {
    let mut tx = Transcript::new();
    let frame = render(
        &mut tx,
        3,
        &[
            ("a", vec!["a".into()], true),
            ("b", vec!["b".into()], true),
            ("c", vec!["c".into()], true),
            ("d", vec!["d".into()], true),
        ],
    );
    assert_eq!(frame.full, vec!["a", "", "b", "", "c", "", "d"]);
    assert_eq!(frame.scrollback, vec!["a", "", "b", ""]);
    assert_eq!(frame.live, vec!["c", "", "d"]);
    assert_eq!(frame.committed_rows, 4);
}

#[test]
fn max_height_truncates_live_block_to_tail() {
    let mut tx = Transcript::new();
    let lines: Vec<String> = (1..=20).map(|n| format!("line {n}")).collect();
    let specs: Vec<BlockSpec<'_>> = vec![BlockSpec {
        key: "live:stream-prose",
        lines: &lines,
        finalized: false,
        max_height: Some(5),
    }];
    let frame = tx.render(20, &specs);
    assert_eq!(frame.full.len(), 6);
    assert!(frame.full[0].contains("15 earlier lines"));
    assert_eq!(frame.full[1], "line 16");
    assert_eq!(frame.full[5], "line 20");
}

#[test]
fn max_height_none_shows_all_lines() {
    let mut tx = Transcript::new();
    let lines: Vec<String> = (1..=20).map(|n| format!("line {n}")).collect();
    let specs: Vec<BlockSpec<'_>> = vec![BlockSpec {
        key: "live:stream-prose",
        lines: &lines,
        finalized: false,
        max_height: None,
    }];
    let frame = tx.render(20, &specs);
    assert_eq!(frame.full.len(), 20);
    assert_eq!(frame.full[0], "line 1");
    assert_eq!(frame.full[19], "line 20");
}

#[test]
fn max_height_under_cap_shows_all_lines() {
    let mut tx = Transcript::new();
    let lines: Vec<String> = (1..=3).map(|n| format!("line {n}")).collect();
    let specs: Vec<BlockSpec<'_>> = vec![BlockSpec {
        key: "live:stream-prose",
        lines: &lines,
        finalized: false,
        max_height: Some(5),
    }];
    let frame = tx.render(20, &specs);
    assert_eq!(frame.full.len(), 3);
    assert_eq!(frame.full[0], "line 1");
}
