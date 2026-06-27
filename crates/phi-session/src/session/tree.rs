//! Tree navigation: parent/child relationships, lineage walks, branch
//! accessors, and user-message enumeration.

use super::types::{Entry, ReplEntry, Role, SessionStore};

impl SessionStore {
    pub fn leaf(&self) -> Option<&str> {
        self.leaf.as_deref()
    }

    /// Whether the session has any message entries. Used by the TUI startup
    /// gate to decide whether to replay the branch (resume) or show the
    /// greeting (new session), independent of the leaf pointer.
    pub fn has_entries(&self) -> bool {
        !self.entries.is_empty()
    }

    pub fn entry(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// Child entries of `parent_id` in file order, via the child index.
    pub(super) fn child_entries(&self, parent_id: &str) -> Vec<&Entry> {
        self.entry_children
            .get(parent_id)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.entries.iter().find(|e| e.id == *id))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Root entries (no parent), via the child index keyed on "" sentinel.
    pub(super) fn root_entries(&self) -> Vec<&Entry> {
        self.entries
            .iter()
            .filter(|e| e.parent_id.is_none())
            .collect()
    }

    /// Child repls of `parent_id` in file order, via the child index.
    pub(super) fn child_repls(&self, parent_id: &str) -> Vec<&ReplEntry> {
        self.repl_children
            .get(parent_id)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.repls.iter().find(|r| r.id == *id))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Lineage of `id` (root → `id`), following parent links.
    pub(super) fn lineage(&self, id: &str) -> Vec<&Entry> {
        let mut chain = Vec::new();
        let mut current = self.entry(id);
        while let Some(entry) = current {
            chain.push(entry);
            current = entry.parent_id.as_deref().and_then(|p| self.entry(p));
        }
        chain.reverse();
        chain
    }

    /// Entries on the active branch, root → leaf.
    pub fn branch(&self) -> Vec<&Entry> {
        self.leaf
            .as_deref()
            .map(|id| self.lineage(id))
            .unwrap_or_default()
    }

    /// All user messages in the session (any branch), chronological — the
    /// /fork candidates, mirroring pi's getUserMessagesForForking.
    pub fn user_messages(&self) -> Vec<&Entry> {
        self.entries
            .iter()
            .filter(|e| e.role == Role::User)
            .collect()
    }

    /// Depth-first walk of the whole entry tree as (depth, entry) rows, in
    /// file order within each sibling group — the /tree view.
    pub fn tree_rows(&self) -> Vec<(usize, &Entry)> {
        let mut rows = Vec::new();
        let roots: Vec<&Entry> = self.root_entries();
        let mut stack: Vec<(usize, &Entry)> = roots.into_iter().rev().map(|e| (0, e)).collect();
        while let Some((depth, entry)) = stack.pop() {
            rows.push((depth, entry));
            let children: Vec<&Entry> = self.child_entries(&entry.id);
            stack.extend(children.into_iter().rev().map(|e| (depth + 1, e)));
        }
        rows
    }
}
