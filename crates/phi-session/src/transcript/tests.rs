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
    // History shrank and the surviving row's content changed under the
    // generation bump. The content-addressed clamp finds the divergence
    // at row 0 (content differs), so commit progress resets to 0 and the
    // new row re-emits as scrollback. (If the surviving content were
    // byte-identical, it would correctly NOT re-emit — see
    // `generation_bump_rebuilds_but_preserves_commit_progress`.)
    let frame = render_gen(&mut tx, 20, 1, &[("a", vec!["a2".into()], true)]);
    assert_eq!(frame.scrollback, vec!["a2"]);
    assert_eq!(frame.committed_rows, 1);
}

#[test]
fn prefix_divergence_without_generation_bump_self_heals() {
    let mut tx = Transcript::new();
    render(&mut tx, 20, &[("a", vec!["a".into()], true)]);
    // Same generation, different key at index 0: the fingerprint mismatch
    // rebuilds the assembly. Content-addressed commit progress finds the
    // divergence at row 0 (the content changed entirely), so both new rows
    // re-emit as scrollback — the previously-committed "a" is stale.
    let frame = render(&mut tx, 20, &[("z", vec!["z1".into(), "z2".into()], true)]);
    assert_eq!(tx.durable_rows(), ["z1", "z2"]);
    assert_eq!(frame.scrollback, vec!["z1", "z2"]);
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
// ── WS6.4: prefix-stability invariant audit ──────────────────────────────
//
// `committed_rows` tracks how many durable-prefix rows have already been
// emitted as terminal scrollback. The invariant it must preserve: live
// (non-finalized) rows are NEVER committed — they belong to the repaintable
// region. If a generation bump rebuilds the durable prefix to fewer rows
// while `committed_rows` still points into the old (now-vanished) prefix,
// the stale commit point can exceed the new durable boundary and swallow
// live rows into "already emitted" scrollback, silently corrupting the
// repaintable region. These tests assert the invariant holds under the
// suspect scenarios from the plan (tool-summary run-merging, cache
// rebuilds, content-only changes).

/// A finalized block whose content changes with the SAME key and SAME row
/// count, without a generation bump, leaves the durable cache stale but
/// does NOT misalign the commit point: `committed_rows` is a row count, and
/// the row count is unchanged. (Display staleness is the caller's job to
/// fix via a generation bump — see `composition.rs`.)
#[test]
fn content_change_same_key_and_len_keeps_commit_point() {
    let mut tx = Transcript::new();
    render(&mut tx, 20, &[("a", vec!["x".into(), "y".into()], true)]);
    assert_eq!(tx.committed_rows(), 2);
    // Same key, same length, different content, NO generation bump: the
    // (key, len) fingerprint still matches, so the cache is trusted. The
    // commit point stays at 2 — no misalignment.
    let frame = render(&mut tx, 20, &[("a", vec!["x".into(), "Z".into()], true)]);
    assert_eq!(frame.scrollback, Vec::<String>::new());
    assert_eq!(frame.committed_rows, 2);
    assert_eq!(frame.live, Vec::<String>::new());
}

/// The bug the plan names: a generation bump rebuilds the durable prefix
/// to FEWER rows while live content keeps the total row count above the
/// stale `committed_rows`. Without the clamp, `committed_rows` exceeds the
/// new durable boundary and the live rows vanish (swallowed as already-
/// emitted scrollback). The invariant — live rows are never committed —
/// must hold, so the live region survives and re-renders.
#[test]
fn gen_bump_shrunken_prefix_with_live_does_not_swallow_live_rows() {
    let mut tx = Transcript::new();
    // Two finalized blocks → durable = [a1, a2, "", b1, b2], all committed.
    render_gen(
        &mut tx,
        20,
        0,
        &[
            ("a", vec!["a1".into(), "a2".into()], true),
            ("b", vec!["b1".into(), "b2".into()], true),
        ],
    );
    assert_eq!(tx.committed_rows(), 5);
    // Generation bump: prefix shrinks to one 1-line block, but a live
    // block keeps total rows >= the stale commit point (5). total = 1
    // (durable) + 1 (sep) + 3 (live) = 5, so the coarse shrink guard
    // (committed > total) does NOT fire. Without the durable-end clamp,
    // committed_rows=5 would skip all live rows.
    let frame = render_gen(
        &mut tx,
        20,
        1,
        &[
            ("a", vec!["a1".into()], true),
            ("live", vec!["l1".into(), "l2".into(), "l3".into()], false),
        ],
    );
    // The durable shrank from [a1,a2,"",b1,b2] to [a1]. The first row
    // (a1) is byte-identical, so the content-addressed clamp keeps commit
    // progress at row 1 (a1 already emitted) — only the durable/live
    // separator re-emits as scrollback. The rows that vanished (a2, the
    // old separator, b1, b2) are gone from the new durable; they remain in
    // the terminal's history but are no longer tracked, so the live region
    // starts cleanly at the new durable boundary.
    assert_eq!(frame.scrollback, vec![""]);
    // The live rows survive — they are NOT swallowed into the committed
    // region.
    assert_eq!(frame.live, vec!["l1", "l2", "l3"]);
    assert_eq!(frame.live_region_start, Some(2));
    // committed_rows now sits at the durable/live boundary (2), never above.
    assert_eq!(frame.committed_rows, 2);
}

/// A tool-summary run-merge: a finalized summary card for a ReplBlock run
/// is replaced (same key, different content) under a generation bump, then
/// the run extends and the card is replaced again. The commit point must
/// stay at the durable boundary each time — never swallowing the live
/// block that follows.
#[test]
fn tool_summary_run_merge_does_not_corrupt_commit_point() {
    let mut tx = Transcript::new();
    // Stable history + a finalized summary card + a live streaming block.
    render_gen(
        &mut tx,
        20,
        0,
        &[
            ("hist", vec!["h".into()], true),
            ("tx:summary:1", vec!["tools: 1".into()], true),
            ("live", vec!["streaming".into()], false),
        ],
    );
    assert_eq!(tx.committed_rows(), 4); // h, "", tools:1, "" (sep before live)
    // The run extends: the summary card is popped and re-rendered with new
    // content under a generation bump (the composition.rs path), and the
    // live block keeps streaming.
    let frame = render_gen(
        &mut tx,
        20,
        1,
        &[
            ("hist", vec!["h".into()], true),
            (
                "tx:summary:1",
                vec!["tools: 2".into(), "repls: 2".into()],
                true,
            ),
            ("live", vec!["streaming".into()], false),
        ],
    );
    // The summary card grew (1 → 2 lines) and its content changed. The
    // content-addressed clamp finds the divergence at row 2 ("tools: 1" →
    // "tools: 2"), so rows 2..4 (the new summary content) re-emit as
    // scrollback along with the durable/live separator, and the commit
    // point advances to the new durable boundary (5) with the live row
    // still repaintable below it.
    assert_eq!(frame.scrollback, vec!["tools: 2", "repls: 2", ""]);
    assert_eq!(frame.live, vec!["streaming"]);
    assert_eq!(frame.live_region_start, Some(5));
    assert_eq!(frame.committed_rows, 5);
}

/// Monotonicity within a generation: `committed_rows` never decreases
/// unless the prefix shrinks below it (shrink guard) or a generation bump
/// rebuilds a smaller durable (clamp). Steady-state re-renders emit no new
// scrollback and leave the commit point untouched.
#[test]
fn committed_rows_monotonic_within_generation() {
    let mut tx = Transcript::new();
    let blocks = [
        ("a", vec!["a".into()], true),
        ("live", vec!["typing".into()], false),
    ];
    let f1 = render(&mut tx, 20, &blocks);
    let f2 = render(&mut tx, 20, &blocks);
    let f3 = render(&mut tx, 20, &blocks);
    assert_eq!(f1.committed_rows, f2.committed_rows);
    assert_eq!(f2.committed_rows, f3.committed_rows);
    assert!(f2.scrollback.is_empty());
    assert!(f3.scrollback.is_empty());
}
