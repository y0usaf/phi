//! Bridges the session tree to the TUI tree browser and back. The browser
//! widget (`phi_tui::tree::TreeBrowser`) is session-free: it reads
//! `TreeViewNode`s + an active leaf id and emits `TreeAction`s. This module
//! adapts `phi_session::TreeNode` → `TreeViewNode` and turns a `TreeAction`
//! into a host-facing `TreeOutcome` the chat acts on (navigate the leaf in
//! place, or open the label editor). It owns no I/O and no session state —
//! the chat owns the store and applies the outcome.

use phi_session::TreeNode;
use phi_tui::tree::{TreeAction, TreeBrowser, TreeMessage, TreeStyle, TreeViewNode};
use serde_json::Value;

// Re-export so the chat can name the browser type without a direct phi_tui
// dependency in its own overlay enum.
pub use phi_tui::tree::TreeBrowser as Browser;

/// What the host should do after the browser emits an action.
#[derive(Debug)]
pub enum TreeOutcome {
    /// Navigate the current session's leaf to `entry_id` in place (pi's
    /// `/tree` Enter — `branch` + rebuild transcript).
    Navigate(String),
    /// Open the label editor seeded with the selected node's current label.
    Label {
        entry_id: String,
        label: Option<String>,
    },
    /// Esc with an empty search: close the tree, restore the editor.
    Cancel,
}

/// Build a `TreeBrowser` from the session tree. The chat reads `store.tree()`
/// + `store.leaf_id()` and hands them here; the chat owns the store.
pub fn browser(tree: &[TreeNode], leaf_id: Option<&str>, height: usize) -> TreeBrowser {
    let nodes: Vec<TreeViewNode> = tree.iter().map(convert_node).collect();
    TreeBrowser::new(
        nodes,
        leaf_id.map(str::to_owned),
        height,
        TreeStyle::default(),
    )
}

/// Run one key through the browser, mapping its `TreeAction` to a
/// `TreeOutcome`. Returns `None` when the key was a pass-through (the
/// browser consumed nothing and emitted no action).
pub fn handle(
    browser: &mut TreeBrowser,
    key: phi_tui::crossterm::event::KeyEvent,
) -> Option<TreeOutcome> {
    match browser.handle_key(key) {
        phi_tui::tree::TreeKey::Handled(Some(action)) => Some(match action {
            TreeAction::Navigate(id) => TreeOutcome::Navigate(id),
            TreeAction::Label { entry_id, label } => TreeOutcome::Label { entry_id, label },
            TreeAction::Cancel => TreeOutcome::Cancel,
        }),
        // Handled(None): the widget re-rendered but wants nothing from the
        // host (selection moved, search typed). The chat still re-renders.
        phi_tui::tree::TreeKey::Handled(None) => None,
        phi_tui::tree::TreeKey::Unhandled => None,
    }
}

/// Render the tree overlay lines: a header, the browser body, and a help
/// footer summarizing the key vocabulary. These replace the chat's live
/// region while the tree is open.
pub fn render(browser: &TreeBrowser, width: usize) -> Vec<String> {
    use phi_tui::style::{DIM, RESET};
    let mut lines: Vec<String> = vec![format!(
        "{DIM}  Session Tree — Esc to close, Enter to navigate, Shift-L to label{RESET}"
    )];
    lines.extend(browser.render(width));
    lines
}

/// Convert a session `TreeNode` to the browser's session-free view. Assistant
/// text + tool calls are extracted so a `ToolResult` renders as `[read: path]`
/// not `[tool]`, matching pi's `toolCallMap`.
fn convert_node(node: &TreeNode) -> TreeViewNode {
    let message = convert_message(&node.message);
    TreeViewNode {
        id: node.fields.id.clone(),
        parent_id: node.fields.parent_id.clone(),
        message,
        label: node.label.clone(),
        label_timestamp: node.label_timestamp.clone(),
        children: node.children.iter().map(convert_node).collect(),
    }
}

fn convert_message(message: &phi_ai::Message) -> TreeMessage {
    match message {
        phi_ai::Message::User(u) => TreeMessage::User(u.content.clone()),
        phi_ai::Message::Assistant(a) => {
            let mut text_parts: Vec<String> = Vec::new();
            let mut tool_calls: Vec<(String, String, Value)> = Vec::new();
            for block in &a.content {
                match block {
                    phi_ai::AssistantContent::Text { text } => text_parts.push(text.clone()),
                    phi_ai::AssistantContent::ToolCall(tc) => {
                        tool_calls.push((tc.id.clone(), tc.name.clone(), tc.arguments.clone()));
                    }
                    phi_ai::AssistantContent::Thinking { .. } => {}
                }
            }
            let text = if text_parts.is_empty() {
                None
            } else {
                Some(text_parts.join("\n"))
            };
            TreeMessage::Assistant {
                text,
                tool_calls,
                stop_reason: Some(format!("{:?}", a.stop_reason)),
                error_message: a.error_message.clone(),
            }
        }
        phi_ai::Message::ToolResult(r) => TreeMessage::ToolResult {
            tool_call_id: Some(r.tool_use_id.clone()),
            tool_name: None,
        },
    }
}
