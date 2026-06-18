    use super::*;
    fn render(
        tx: &mut Transcript,
        width: usize,
        viewport_height: usize,
        inputs: &[(&str, Vec<String>, bool, bool)],
    ) -> Frame {
        let specs: Vec<BlockSpec<'_>> = inputs
            .iter()
            .map(|(key, lines, finalized, commit_stable)| BlockSpec {
                key,
                lines,
                finalized: *finalized,
                commit_stable: *commit_stable,
                version: 0,
            })
            .collect();
        tx.render(width, viewport_height, &specs)
    }

    #[test]
    fn exactly_one_blank_separator_between_visible_blocks() {
        let mut tx = Transcript::new();
        let frame = render(
            &mut tx,
            80,
            20,
            &[
                ("a", vec!["".into(), "a".into(), "".into()], true, true),
                ("b", vec!["b".into()], true, true),
            ],
        );
        assert_eq!(frame.full, vec!["a", "", "b"]);
    }

    #[test]
    fn earliest_unfinalized_block_starts_live_region() {
        let mut tx = Transcript::new();
        let frame = render(
            &mut tx,
            80,
            20,
            &[
                ("done", vec!["done".into()], true, true),
                ("pending", vec!["pending".into()], false, true),
                ("card", vec!["card".into()], true, true),
            ],
        );
        assert_eq!(frame.full, vec!["done", "", "pending", "", "card"]);
        assert_eq!(frame.live_region_start, Some(2));
    }

    #[test]
    fn provisional_live_block_never_offers_snapshot_beyond_safe_prefix() {
        let mut tx = Transcript::new();
        for _ in 0..40 {
            let frame = render(
                &mut tx,
                80,
                20,
                &[
                    ("history", vec!["history".into()], true, true),
                    (
                        "preview",
                        vec![
                            "┌ Edit: foo.ts".into(),
                            "… (2 more hunks above)".into(),
                            "-old-a".into(),
                            "+new-a".into(),
                            "└ (streaming)".into(),
                        ],
                        false,
                        false,
                    ),
                ],
            );
            assert_eq!(frame.live_region_start, Some(2));
            assert_eq!(frame.commit_safe_end, None);
            assert_eq!(frame.snapshot_safe_end, None);
        }
    }

    #[test]
    fn trailing_line_growth_is_offerable() {
        let mut tx = Transcript::new();
        let first = render(
            &mut tx,
            80,
            20,
            &[(
                "stream",
                vec![
                    "para one".into(),
                    "para two".into(),
                    "the quick brown".into(),
                ],
                false,
                true,
            )],
        );
        assert_eq!(first.commit_safe_end, None);

        let second = render(
            &mut tx,
            80,
            20,
            &[(
                "stream",
                vec![
                    "para one".into(),
                    "para two".into(),
                    "the quick brown fox".into(),
                ],
                false,
                true,
            )],
        );
        assert_eq!(second.commit_safe_end, Some(3));
    }

    #[test]
    fn snapshot_safe_live_rows_do_not_enter_terminal_scrollback() {
        let mut tx = Transcript::new_embedded();
        let frame = render(
            &mut tx,
            80,
            3,
            &[(
                "stream",
                vec![
                    "one".into(),
                    "two".into(),
                    "three".into(),
                    "four".into(),
                    "five".into(),
                ],
                false,
                true,
            )],
        );

        assert_eq!(frame.commit_safe_end, None);
        assert_eq!(frame.snapshot_safe_end, Some(5));
        assert!(frame.scrollback.is_empty());
        assert_eq!(frame.committed_rows, 0);
    }

    #[test]
    fn finalized_history_before_unsafe_live_rows_can_still_scroll() {
        let mut tx = Transcript::new_embedded();
        let frame = render(
            &mut tx,
            80,
            3,
            &[
                ("history", vec!["history".into()], true, true),
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
                    true,
                ),
            ],
        );

        assert_eq!(frame.commit_safe_end, None);
        assert_eq!(frame.snapshot_safe_end, Some(7));
        assert_eq!(frame.scrollback, vec!["history", ""]);
        assert_eq!(frame.committed_rows, 2);
    }

    #[test]
    fn viewport_commits_scrolled_safe_rows_only() {
        let mut tx = Transcript::new();
        let frame = render(
            &mut tx,
            80,
            3,
            &[
                ("a", vec!["a".into()], true, true),
                ("b", vec!["b".into()], true, true),
                ("c", vec!["c".into()], true, true),
                ("d", vec!["d".into()], true, true),
            ],
        );
        assert_eq!(frame.full, vec!["a", "", "b", "", "c", "", "d"]);
        assert_eq!(frame.scrollback, vec!["a", "", "b", ""]);
        assert_eq!(frame.live, vec!["c", "", "d"]);
        assert_eq!(frame.committed_rows, 4);
    }
