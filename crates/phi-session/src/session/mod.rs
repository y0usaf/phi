//! Persistent sessions (port of pi's session-manager): append-only
//! JSONL files where message entries carry id/parent_id, forming a branch
//! tree. Files live under the phi data dir, in a per-cwd folder. Files are
//! created lazily on the first message, so empty sessions leave no trace.

mod discovery;
mod reconstruct;
mod store;
mod tree;
mod types;

pub use discovery::age;
pub use store::{flatten_cwd, sessions_dir, sessions_root};
pub use types::{
    BranchExchange, BranchItem, Entry, ExchangeEnd, ExchangeStart, ExchangeStatus, ReplEntry,
    ReplKind, ReplRecord, Role, SessionMeta, SessionStore, SessionUsage, SubcallRecord, ToolCall,
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn store_in(dir: &Path) -> SessionStore {
        let mut store = SessionStore::new(dir).unwrap();
        // Redirect under the temp dir so tests never touch the real data dir.
        store.path = dir.join(format!("{}.jsonl", store.header.id));
        store
    }

    #[test]
    fn append_load_roundtrip_preserves_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = store_in(tmp.path());
        let u1 = store.append(Role::User, "first prompt").unwrap();
        store.append(Role::Assistant, "first answer").unwrap();
        store.append(Role::User, "second prompt").unwrap();
        store.append(Role::Assistant, "second answer").unwrap();

        let loaded = SessionStore::load(store.path()).unwrap();
        assert_eq!(loaded.entries.len(), 4);
        assert_eq!(loaded.branch().len(), 4);
        let (exchanges, pending) = loaded.branch_restore();
        assert!(pending.is_empty());
        assert_eq!(
            exchanges
                .iter()
                .map(|x| (x.prompt.as_str(), x.response.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("first prompt", "first answer"),
                ("second prompt", "second answer"),
            ]
        );
        assert_eq!(loaded.entry(&u1).unwrap().text, "first prompt");
    }

    #[test]
    fn set_leaf_branches_the_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = store_in(tmp.path());
        store.append(Role::User, "q1").unwrap();
        let a1 = store.append(Role::Assistant, "a1").unwrap();
        store.append(Role::User, "q2").unwrap();
        store.append(Role::Assistant, "a2").unwrap();

        // Rewind to a1 and take a different second turn.
        store.set_leaf(Some(a1.clone())).unwrap();
        store.append(Role::User, "q2-alt").unwrap();
        store.append(Role::Assistant, "a2-alt").unwrap();

        let (exchanges, _) = store.branch_restore();
        assert_eq!(
            exchanges
                .iter()
                .map(|x| (x.prompt.as_str(), x.response.as_str()))
                .collect::<Vec<_>>(),
            vec![("q1", "a1"), ("q2-alt", "a2-alt")]
        );
        // The tree keeps both branches: a1 has two children.
        let rows = store.tree_rows();
        assert_eq!(rows.len(), 6);
        let a1_children = rows
            .iter()
            .filter(|(_, e)| e.parent_id.as_deref() == Some(a1.as_str()));
        assert_eq!(a1_children.count(), 2);
        // Unpaired user prompts are skipped in exchanges.
        store
            .set_leaf(store.user_messages().first().map(|e| e.id.clone()))
            .unwrap();
        assert!(store.branch_restore().0.is_empty());
    }

    #[test]
    fn fork_copies_lineage_before_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = store_in(tmp.path());
        store.append(Role::User, "q1").unwrap();
        store.append(Role::Assistant, "a1").unwrap();
        let q2 = store.append(Role::User, "q2").unwrap();
        store.append(Role::Assistant, "a2").unwrap();

        let fork = store.fork(&q2).unwrap();
        assert!(fork.path().starts_with(tmp.path()));
        let (exchanges, _) = fork.branch_restore();
        assert_eq!(
            exchanges
                .iter()
                .map(|x| (x.prompt.as_str(), x.response.as_str()))
                .collect::<Vec<_>>(),
            vec![("q1", "a1")]
        );
        // Forking from the first message yields an empty session.
        let first = store.user_messages()[0].id.clone();
        let empty = store.fork(&first).unwrap();
        assert!(empty.branch().is_empty());
    }

    #[test]
    fn repl_entries_roundtrip_and_follow_parent_order() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = store_in(tmp.path());
        store.append(Role::User, "q1").unwrap();
        let a1 = store.append(Role::Assistant, "a1").unwrap();
        store
            .append_repl(ReplRecord {
                code: "print(1)".to_owned(),
                stdout: "1
"
                .to_owned(),
                stderr: String::new(),
                duration_secs: 0.5,
                kind: ReplKind::Agent,
                tools: vec![ToolCall {
                    name: "shell".to_owned(),
                    args: serde_json::json!({"cmd": "ls"}),
                    result: "ok".to_owned(),
                    error: None,
                    duration_secs: 0.1,
                    depth: 0,
                }],
                subcalls: vec![SubcallRecord {
                    model: "haiku".to_owned(),
                    prompt_preview: "summarize".to_owned(),
                    result_preview: Some("done".to_owned()),
                    error: None,
                    duration_secs: 0.2,
                    depth: 0,
                }],
            })
            .unwrap();
        store.append(Role::User, "q2").unwrap();
        store.append(Role::Assistant, "a2").unwrap();

        let loaded = SessionStore::load(store.path()).unwrap();
        let items = loaded.branch_items();
        assert_eq!(items.len(), 5);
        match items[1] {
            BranchItem::Message(entry) => assert_eq!(entry.id, a1),
            _ => panic!("expected assistant message second"),
        }
        match items[2] {
            BranchItem::Repl(repl) => {
                assert_eq!(repl.code, "print(1)");
                assert_eq!(
                    repl.stdout,
                    "1
"
                );
                assert_eq!(repl.parent_id.as_deref(), Some(a1.as_str()));
                assert_eq!(repl.kind, ReplKind::Agent);
                assert_eq!(repl.tools.len(), 1);
                assert_eq!(repl.tools[0].name, "shell");
                assert_eq!(repl.tools[0].result, "ok");
                assert_eq!(repl.tools[0].depth, 0);
                assert_eq!(repl.subcalls.len(), 1);
                assert_eq!(repl.subcalls[0].model, "haiku");
                assert_eq!(repl.subcalls[0].depth, 0);
            }
            _ => panic!("expected repl after parent assistant"),
        }

        let (turns, trailing) = loaded.branch_exchanges();
        assert!(trailing.is_empty());
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].response.unwrap().id, a1);
        assert_eq!(turns[0].repls.len(), 1);
        assert_eq!(turns[0].repls[0].parent_id.as_deref(), Some(a1.as_str()));
    }

    #[test]
    fn old_repl_activity_records_default_to_root_depth() {
        let tool: ToolCall = serde_json::from_value(serde_json::json!({
            "name": "shell",
            "args": {"cmd": "echo hi"},
            "result": "hi\n",
            "error": null,
            "duration_secs": 0.1
        }))
        .unwrap();
        assert_eq!(tool.depth, 0);

        let subcall: SubcallRecord = serde_json::from_value(serde_json::json!({
            "model": "sub",
            "prompt_preview": "say hi",
            "result_preview": "hi",
            "duration_secs": 0.2
        }))
        .unwrap();
        assert_eq!(subcall.depth, 0);
    }

    #[test]
    fn repl_activity_depth_roundtrips() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = store_in(tmp.path());
        store.append(Role::User, "q1").unwrap();
        store
            .append_repl(ReplRecord {
                code: "print(llm_query('x'))".to_owned(),
                stdout: "done\n".to_owned(),
                stderr: String::new(),
                duration_secs: 0.5,
                kind: ReplKind::Agent,
                tools: vec![ToolCall {
                    name: "shell".to_owned(),
                    args: serde_json::json!({"cmd": "echo nested"}),
                    result: "nested\n".to_owned(),
                    error: None,
                    duration_secs: 0.1,
                    depth: 1,
                }],
                subcalls: vec![SubcallRecord {
                    model: "sub".to_owned(),
                    prompt_preview: "x".to_owned(),
                    result_preview: Some("y".to_owned()),
                    error: None,
                    duration_secs: 0.2,
                    depth: 1,
                }],
            })
            .unwrap();

        let loaded = SessionStore::load(store.path()).unwrap();
        let repl = loaded
            .branch_items()
            .into_iter()
            .find_map(|item| match item {
                BranchItem::Repl(repl) => Some(repl),
                BranchItem::Message(_) => None,
            })
            .expect("repl item");
        assert_eq!(repl.tools[0].depth, 1);
        assert_eq!(repl.subcalls[0].depth, 1);
    }

    #[test]
    fn user_parented_agent_repls_group_into_the_following_exchange() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = store_in(tmp.path());
        let q1 = store.append(Role::User, "q1").unwrap();
        store
            .append_repl(ReplRecord {
                code: "print(1)".to_owned(),
                stdout: "1
"
                .to_owned(),
                stderr: String::new(),
                duration_secs: 0.5,
                kind: ReplKind::Agent,
                tools: Vec::new(),
                subcalls: Vec::new(),
            })
            .unwrap();
        let a1 = store
            .append(Role::Assistant, "before\n```repl\nprint(1)\n```\nafter")
            .unwrap();

        let loaded = SessionStore::load(store.path()).unwrap();

        let items = loaded.branch_items();
        assert_eq!(items.len(), 3);
        match items[0] {
            BranchItem::Message(entry) => assert_eq!(entry.id, q1),
            _ => panic!("expected user message first"),
        }
        match items[1] {
            BranchItem::Repl(repl) => assert_eq!(repl.parent_id.as_deref(), Some(q1.as_str())),
            _ => panic!("expected user-parented repl second"),
        }
        match items[2] {
            BranchItem::Message(entry) => assert_eq!(entry.id, a1),
            _ => panic!("expected assistant message third"),
        }

        let (turns, trailing) = loaded.branch_exchanges();
        assert!(trailing.is_empty());
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].prompt.id, q1);
        assert_eq!(turns[0].response.unwrap().id, a1);
        assert_eq!(turns[0].repls.len(), 1);
        assert_eq!(turns[0].repls[0].code, "print(1)");
        assert_eq!(turns[0].repls[0].parent_id.as_deref(), Some(q1.as_str()));

        let (exchanges, pending) = loaded.branch_restore();
        assert!(pending.is_empty());
        assert_eq!(exchanges.len(), 1);
        assert_eq!(exchanges[0].prompt, "q1");
        assert_eq!(
            exchanges[0].response,
            "before\n```repl\nprint(1)\n```\nafter"
        );
        assert_eq!(exchanges[0].repls.len(), 1);
        assert_eq!(exchanges[0].repls[0].code, "print(1)");
    }

    #[test]
    fn assistant_display_text_roundtrips_but_restore_uses_semantic_text() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = store_in(tmp.path());
        let q1 = store.append(Role::User, "q1").unwrap();
        store
            .append_repl(ReplRecord {
                code: "answer['content'] = 'done'\nanswer['ready'] = True".to_owned(),
                stdout: String::new(),
                stderr: String::new(),
                duration_secs: 0.1,
                kind: ReplKind::Agent,
                tools: Vec::new(),
                subcalls: Vec::new(),
            })
            .unwrap();
        store
            .append_entry(
                Role::Assistant,
                "done",
                Some(
                    "Submitting.\n```repl\nanswer['content'] = 'done'\nanswer['ready'] = True\n```done"
                        .to_owned(),
                ),
                None,
                Vec::new(),
            )
            .unwrap();

        let loaded = SessionStore::load(store.path()).unwrap();
        let (turns, trailing) = loaded.branch_exchanges();
        assert!(trailing.is_empty());
        assert_eq!(turns.len(), 1);
        let response = turns[0].response.unwrap();
        assert_eq!(response.text, "done");
        assert_eq!(
            response.display_text.as_deref(),
            Some(
                "Submitting.\n```repl\nanswer['content'] = 'done'\nanswer['ready'] = True\n```done"
            )
        );
        assert_eq!(turns[0].repls.len(), 1);
        assert_eq!(turns[0].repls[0].parent_id.as_deref(), Some(q1.as_str()));

        let (exchanges, pending) = loaded.branch_restore();
        assert!(pending.is_empty());
        assert_eq!(exchanges.len(), 1);
        assert_eq!(exchanges[0].prompt, "q1");
        assert_eq!(exchanges[0].response, "done");
    }

    #[test]
    fn open_exchange_is_detected_as_interrupted_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store_in(dir.path());
        store.append(Role::User, "q1").unwrap();
        store.begin_exchange().unwrap();
        // Crash here: no exchange_end. A reload must surface it.
        let mut loaded = SessionStore::load(store.path()).unwrap();
        assert!(loaded.interrupted_exchange().is_some());
        loaded.acknowledge_interrupted().unwrap();
        assert!(loaded.interrupted_exchange().is_none());
        // The acknowledgement is durable: a second load sees a closed log.
        let reloaded = SessionStore::load(store.path()).unwrap();
        assert!(reloaded.interrupted_exchange().is_none());
    }

    #[test]
    fn closed_exchange_is_not_interrupted_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store_in(dir.path());
        store.append(Role::User, "q1").unwrap();
        store.begin_exchange().unwrap();
        store.append(Role::Assistant, "a1").unwrap();
        store.end_exchange(ExchangeStatus::Completed).unwrap();
        let loaded = SessionStore::load(store.path()).unwrap();
        assert!(loaded.interrupted_exchange().is_none());
    }

    #[test]
    fn set_leaf_persists_across_reload() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store_in(dir.path());
        store.append(Role::User, "q1").unwrap();
        let a1 = store.append(Role::Assistant, "a1").unwrap();
        store.append(Role::User, "q2").unwrap();
        store.append(Role::Assistant, "a2").unwrap();

        // Rewind to a1: resume must return here, not to the newest entry.
        store.set_leaf(Some(a1.clone())).unwrap();
        let loaded = SessionStore::load(store.path()).unwrap();
        assert_eq!(loaded.leaf(), Some(a1.as_str()));
        assert_eq!(loaded.branch().len(), 2);
    }

    #[test]
    fn load_survives_torn_tail_and_bad_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store_in(dir.path());
        store.append(Role::User, "q1").unwrap();
        store.append(Role::Assistant, "a1").unwrap();

        // Simulate a crash mid-append (torn final line) plus a line of
        // non-UTF8 garbage: both must be skipped, not fatal.
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(store.path())
            .unwrap();
        file.write_all(&[0xff, 0xfe, b'{', b'\n']).unwrap();
        file.write_all(b"{\"type\":\"message\",\"id\":\"tor")
            .unwrap();
        drop(file);

        let loaded = SessionStore::load(store.path()).unwrap();
        let branch = loaded.branch();
        assert_eq!(branch.len(), 2);
        assert_eq!(branch[1].text, "a1");
    }

    #[test]
    fn append_after_navigation_survives_reload() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = store_in(tmp.path());
        store.append(Role::User, "q1").unwrap();
        let a1 = store.append(Role::Assistant, "a1").unwrap();
        store.append(Role::User, "q2").unwrap();
        store.append(Role::Assistant, "a2").unwrap();

        // Navigate back to a1 and take a different second turn.
        store.set_leaf(Some(a1.clone())).unwrap();
        store.append(Role::User, "q2-alt").unwrap();
        store.append(Role::Assistant, "a2-alt").unwrap();

        // In-memory state is correct.
        let (exchanges, _) = store.branch_restore();
        assert_eq!(
            exchanges
                .iter()
                .map(|x| x.prompt.as_str())
                .collect::<Vec<_>>(),
            vec!["q1", "q2-alt"]
        );

        // After reload, the side branch must survive — the leaf must point
        // at a2-alt, not at the stale a1 leaf record.
        let loaded = SessionStore::load(store.path()).unwrap();
        let (loaded_exchanges, _) = loaded.branch_restore();
        assert_eq!(
            loaded_exchanges
                .iter()
                .map(|x| (x.prompt.as_str(), x.response.as_str()))
                .collect::<Vec<_>>(),
            vec![("q1", "a1"), ("q2-alt", "a2-alt")],
            "reload must preserve the most recent branch, not the stale leaf"
        );
    }
}
