//! Conversation replay/reconstruction: active-branch transcript items,
//! reconstructed exchanges, and model-state restore.

use super::types::{BranchExchange, BranchItem, ReplEntry, ReplKind, Role, SessionStore};

impl SessionStore {
    /// Active-branch transcript items in JSONL/file order, interleaving
    /// assistant/user messages with the persisted REPL executions attached
    /// to the active branch lineage.
    pub fn branch_items(&self) -> Vec<BranchItem<'_>> {
        let branch = self.branch();
        if branch.is_empty() {
            return Vec::new();
        }
        let branch_ids: std::collections::HashSet<&str> =
            branch.iter().map(|e| e.id.as_str()).collect();
        let mut items = Vec::new();
        for entry in &self.entries {
            if branch_ids.contains(entry.id.as_str()) {
                items.push(BranchItem::Message(entry));
            }
            items.extend(
                self.child_repls(&entry.id)
                    .into_iter()
                    .map(BranchItem::Repl),
            );
        }
        items
    }

    /// Active-branch turns reconstructed from the durable message/repl log.
    ///
    /// Agent REPL checkpoints may be parented either to the turn's user
    /// entry (current live checkpointing) or to its assistant entry (older
    /// tests/manual writes), so replay/restore must group both into one
    /// exchange instead of trusting flat parent order alone. Shell REPLs
    /// between turns become `pre_shell` on the following exchange; any
    /// trailing shell executions are returned separately.
    pub fn branch_exchanges(&self) -> (Vec<BranchExchange<'_>>, Vec<&ReplEntry>) {
        let branch = self.branch();
        if branch.is_empty() {
            return (Vec::new(), Vec::new());
        }
        let mut exchanges = Vec::new();
        let mut shell_buf = Vec::new();
        let mut open: Option<BranchExchange<'_>> = None;
        for entry in branch {
            let attached: Vec<&ReplEntry> = self.child_repls(&entry.id);
            match entry.role {
                Role::User => {
                    if let Some(exchange) = open.take() {
                        exchanges.push(exchange);
                    }
                    let mut exchange = BranchExchange {
                        prompt: entry,
                        response: None,
                        pre_shell: std::mem::take(&mut shell_buf),
                        repls: Vec::new(),
                    };
                    exchange.repls.extend(attached);
                    open = Some(exchange);
                }
                Role::Assistant => {
                    if let Some(mut exchange) = open.take() {
                        exchange.response = Some(entry);
                        for repl in attached {
                            match repl.kind {
                                ReplKind::Agent => exchange.repls.push(repl),
                                ReplKind::Shell => shell_buf.push(repl),
                            }
                        }
                        exchanges.push(exchange);
                    } else {
                        for repl in attached {
                            if repl.kind == ReplKind::Shell {
                                shell_buf.push(repl);
                            }
                        }
                    }
                }
            }
        }
        if let Some(exchange) = open.take() {
            exchanges.push(exchange);
        }
        (exchanges, shell_buf)
    }

    /// The active branch projected for model-state restore: pi-style, the
    /// canonical stream is resolved into typed exchanges carrying the REPL
    /// executions that ran inside each turn, plus any trailing user shell
    /// executions not yet replayed to the model.
    pub fn branch_restore(&self) -> (Vec<phi_rlm::RestoredExchange>, Vec<phi_rlm::RestoredRepl>) {
        fn restored(repl: &ReplEntry) -> phi_rlm::RestoredRepl {
            phi_rlm::RestoredRepl {
                prose: repl.prose.clone(),
                code: repl.code.clone(),
                stdout: repl.stdout.clone(),
                stderr: repl.stderr.clone(),
                subcalls: repl
                    .subcalls
                    .iter()
                    .map(|sc| phi_rlm::RestoredSubcall {
                        model: sc.model.clone(),
                        prompt_preview: sc.prompt_preview.clone(),
                        result: sc.result.clone(),
                        error: sc.error.clone(),
                        depth: sc.depth,
                    })
                    .collect(),
            }
        }
        let (branch, trailing_shell) = self.branch_exchanges();
        let exchanges = branch
            .into_iter()
            .filter_map(|exchange| {
                let response = exchange.response?;
                Some(phi_rlm::RestoredExchange {
                    pre_shell: exchange.pre_shell.into_iter().map(restored).collect(),
                    prompt: exchange.prompt.text.clone(),
                    repls: exchange.repls.into_iter().map(restored).collect(),
                    response: response.text.clone(),
                })
            })
            .collect();
        let pending = trailing_shell
            .into_iter()
            .filter(|repl| repl.kind == ReplKind::Shell)
            .map(restored)
            .collect();
        (exchanges, pending)
    }
}
