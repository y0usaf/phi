use super::*;

fn render(
    tx: &mut Transcript,
    viewport_height: usize,
    inputs: &[(&str, Vec<String>, bool)],
) -> Frame {
    render_gen(tx, viewport_height, 0, inputs)
}

fn render_gen(
    tx: &mut Transcript,
    viewport_height: usize,
    generation: u64,
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
    tx.render(viewport_height, generation, &specs)
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
    assert_eq!(tx.durable_rows(), ["a", "", "b"]);
    assert_eq!(frame.scrollback, vec!["a", "", "b"]);
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
    assert_eq!(frame.scrollback, vec!["done", ""]);
    assert_eq!(frame.live, vec!["pending", "", "card"]);
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
fn finalized_blocks_flush_to_scrollback_immediately() {
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
    assert_eq!(frame.scrollback, vec!["a", "", "b", "", "c", "", "d"]);
    assert_eq!(frame.live, Vec::<String>::new());
    assert_eq!(frame.committed_rows, 7);
}

#[test]
fn steady_state_render_emits_no_new_scrollback() {
    let mut tx = Transcript::new();
    let blocks = [
        ("a", vec!["a".into()], true),
        ("live", vec!["typing...".into()], false),
    ];
    let first = render(&mut tx, 20, &blocks);
    assert_eq!(first.scrollback, vec!["a", ""]);
    let second = render(&mut tx, 20, &blocks);
    assert_eq!(second.scrollback, Vec::<String>::new());
    assert_eq!(second.live, vec!["typing..."]);
    assert_eq!(second.committed_rows, first.committed_rows);
}

#[test]
fn live_block_finalizing_commits_only_its_rows() {
    let mut tx = Transcript::new();
    let frame = render(
        &mut tx,
        20,
        &[
            ("done", vec!["done".into()], true),
            ("pending", vec!["pending".into()], false),
        ],
    );
    assert_eq!(frame.scrollback, vec!["done", ""]);
    // The pending block resolves: its rows (and only its rows) flush, at
    // the exact position the separator was already committed for.
    let frame = render(
        &mut tx,
        20,
        &[
            ("done", vec!["done".into()], true),
            ("pending", vec!["pending".into()], true),
        ],
    );
    assert_eq!(frame.scrollback, vec!["pending"]);
    assert_eq!(frame.live, Vec::<String>::new());
    assert_eq!(tx.durable_rows(), ["done", "", "pending"]);
}

#[test]
fn appended_finalized_block_extends_durable_cache() {
    let mut tx = Transcript::new();
    render(&mut tx, 20, &[("a", vec!["a".into()], true)]);
    let frame = render(
        &mut tx,
        20,
        &[("a", vec!["a".into()], true), ("b", vec!["b".into()], true)],
    );
    assert_eq!(frame.scrollback, vec!["", "b"]);
    assert_eq!(tx.durable_rows(), ["a", "", "b"]);
}

#[test]
fn generation_bump_rebuilds_but_preserves_commit_progress() {
    let mut tx = Transcript::new();
    render_gen(&mut tx, 20, 0, &[("a", vec!["a".into()], true)]);
    assert_eq!(tx.committed_rows(), 1);
    // Same content re-supplied under a new generation (e.g. an expand
    // toggle that happened to render identically): no duplicate scrollback.
    let frame = render_gen(&mut tx, 20, 1, &[("a", vec!["a".into()], true)]);
    assert_eq!(frame.scrollback, Vec::<String>::new());
    assert_eq!(frame.committed_rows, 1);
    assert_eq!(tx.durable_rows(), ["a"]);
}

#[test]
fn shrunken_content_resets_and_reemits() {
    let mut tx = Transcript::new();
    render_gen(
        &mut tx,
        20,
        0,
        &[("a", vec!["a".into()], true), ("b", vec!["b".into()], true)],
    );
    assert_eq!(tx.committed_rows(), 3);
    // History shrank below the committed row count: commit progress resets
    // and the remaining rows re-emit (the pre-existing shrink guard).
    let frame = render_gen(&mut tx, 20, 1, &[("a", vec!["a".into()], true)]);
    assert_eq!(frame.scrollback, vec!["a"]);
    assert_eq!(frame.committed_rows, 1);
}

#[test]
fn prefix_divergence_without_generation_bump_self_heals() {
    let mut tx = Transcript::new();
    render(&mut tx, 20, &[("a", vec!["a".into()], true)]);
    // Same generation, different key at index 0: the fingerprint mismatch
    // rebuilds the assembly instead of trusting the stale cache.
    let frame = render(&mut tx, 20, &[("z", vec!["z1".into(), "z2".into()], true)]);
    assert_eq!(tx.durable_rows(), ["z1", "z2"]);
    assert_eq!(frame.scrollback, vec!["z2"]);
}

#[test]
fn reset_reemits_everything_as_scrollback() {
    let mut tx = Transcript::new();
    let blocks = [("a", vec!["a".into()], true), ("b", vec!["b".into()], true)];
    render(&mut tx, 20, &blocks);
    tx.reset();
    let frame = render(&mut tx, 20, &blocks);
    assert_eq!(frame.scrollback, vec!["a", "", "b"]);
}
