
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
