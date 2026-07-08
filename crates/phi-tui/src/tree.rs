//! Session-tree browser — a faithful port of pi's `TreeSelectorComponent`
//! (`ref/pi/.../components/tree-selector.ts`). Renders the session as an
//! ASCII-art tree (├─ └─ │ connectors with gutter guides, fold markers, an
//! active-path dot, and a one-line `[label]`), and handles the full key
//! vocabulary pi's does: arrow/page navigation, branch fold/unfold +
//! segment jumping, five filter modes (cycled or direct), incremental
//! search, label editing, and a label-timestamp toggle.
//!
//! Pure widget: it owns no I/O and no session state. The host hands it a
//! `Vec<TreeViewNode>` + the active leaf id, drives it with
//! [`TreeBrowser::handle_key`], reads [`TreeBrowser::render`] lines, and
//! acts on the [`TreeAction`] the widget emits (navigate, label, or
//! cancel). The host owns the session store and applies the action; the
//! widget only reads immutable inputs and returns queued actions — the
//! snapshot-in/actions-out doctrine.

use std::collections::{HashMap, HashSet};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::Value;

use crate::cells::truncate_to_cells;
use crate::style::{BOLD, CYAN, DIM, GREEN, RESET};

/// A tree node, decoupled from `phi_session::TreeNode` so this crate stays
/// session-free (the host adapts). Carries the minimum the renderer needs:
/// an id, optional parent, the message to summarize, an optional label,
/// and children.
#[derive(Clone, Debug)]
pub struct TreeViewNode {
    pub id: String,
    pub parent_id: Option<String>,
    pub message: TreeMessage,
    /// User-defined label resolved from the latest label entry.
    pub label: Option<String>,
    /// ISO-8601 timestamp of the latest label change, for the toggle.
    pub label_timestamp: Option<String>,
    pub children: Vec<TreeViewNode>,
}

/// The message flavour a node renders. Maps 1:1 to phi's `Message` roles;
/// the browser summarizes each for the one-line tree view.
#[derive(Clone, Debug)]
pub enum TreeMessage {
    User(String),
    Assistant {
        text: Option<String>,
        /// Tool calls this assistant turn issued (id → name/args), so a
        /// following `ToolResult` node renders `[read: path]` not `[tool]`.
        tool_calls: Vec<(String, String, Value)>,
        stop_reason: Option<String>,
        error_message: Option<String>,
    },
    ToolResult {
        tool_call_id: Option<String>,
        tool_name: Option<String>,
    },
}

/// Filter mode — pi's five, ported verbatim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterMode {
    Default,
    NoTools,
    UserOnly,
    LabeledOnly,
    All,
}

impl FilterMode {
    const CYCLE: [FilterMode; 5] = [
        FilterMode::Default,
        FilterMode::NoTools,
        FilterMode::UserOnly,
        FilterMode::LabeledOnly,
        FilterMode::All,
    ];
    fn next(self) -> Self {
        let i = Self::CYCLE.iter().position(|m| *m == self).unwrap_or(0);
        Self::CYCLE[(i + 1) % Self::CYCLE.len()]
    }
    fn prev(self) -> Self {
        let i = Self::CYCLE.iter().position(|m| *m == self).unwrap_or(0);
        Self::CYCLE[(i + 5 - 1) % Self::CYCLE.len()]
    }
    fn label(self) -> &'static str {
        match self {
            FilterMode::Default => "",
            FilterMode::NoTools => " [no-tools]",
            FilterMode::UserOnly => " [user]",
            FilterMode::LabeledOnly => " [labeled]",
            FilterMode::All => " [all]",
        }
    }
}

/// What the widget asks the host to do. The host applies these against the
/// real session store and re-creates the browser with fresh state.
#[derive(Clone, Debug)]
pub enum TreeAction {
    /// Navigate the leaf to `entry_id` (a branch). The host branches,
    /// rebuilds the transcript, and closes the tree.
    Navigate(String),
    /// Set (or clear, when `None`) a label on `entry_id`. The host opens its
    /// label editor seeded with the current label; `None` here means "open
    /// the editor" (the host decides clear vs edit from the editor result).
    Label {
        entry_id: String,
        label: Option<String>,
    },
    /// Esc with an empty search: close the tree.
    Cancel,
}

/// The outcome of one key: the widget acted (re-render) and may have
/// produced an action for the host, or it consumed nothing (pass-through).
#[derive(Debug)]
pub enum TreeKey {
    Handled(Option<TreeAction>),
    Unhandled,
}

/// Theme-derived ANSI escapes for the tree's color roles. The host builds
/// this from its palette so the tree matches the rest of the chrome.
#[derive(Clone)]
pub struct TreeStyle {
    pub accent: &'static str,
    pub success: &'static str,
    pub muted: &'static str,
    pub warning: &'static str,
    pub error: &'static str,
    pub selected_bg: &'static str,
}

impl Default for TreeStyle {
    fn default() -> Self {
        Self {
            accent: CYAN,
            success: GREEN,
            muted: DIM,
            warning: "\x1b[33m",
            error: "\x1b[31m",
            selected_bg: "\x1b[7m",
        }
    }
}

/// A flattened node: visual metadata the renderer recomputes on every
/// filter/fold change (indent, connector, gutters). `src` indexes the
/// pre-order source list so the message/label come from the immutable tree.
#[derive(Clone)]
struct FlatNode {
    /// Index into `src` (the pre-order source list).
    src: usize,
    indent: usize,
    show_connector: bool,
    is_last: bool,
    gutters: Vec<GutterInfo>,
    is_virtual_root_child: bool,
}

#[derive(Clone, Copy)]
struct GutterInfo {
    /// Indent level where this branch's connector was shown.
    position: usize,
    /// true = continue the `│` guide, false = blank.
    show: bool,
}

/// One pre-order source entry: the node's id, parent id, a borrow-free
/// snapshot of the message/label, and child source indices (source order).
struct SrcEntry {
    id: String,
    parent_id: Option<String>,
    message: TreeMessage,
    label: Option<String>,
    label_timestamp: Option<String>,
    children: Vec<usize>,
}

/// The session-tree browser. Construct once per overlay show from the
/// current tree + leaf; drive with keys; read lines. Mirrors pi's
/// `TreeList` field-for-field where Rust allows it.
pub struct TreeBrowser {
    /// Pre-order source list (immutable for the browser's lifetime).
    src: Vec<SrcEntry>,
    id_to_src: HashMap<String, usize>,
    flat: Vec<FlatNode>,
    filtered: Vec<usize>,
    selected: usize,
    leaf_id: Option<String>,
    max_visible: usize,
    filter_mode: FilterMode,
    search: String,
    multiple_roots: bool,
    show_label_timestamps: bool,
    /// Entry ids on the root→leaf path, for the active-path dot.
    active_path: HashSet<String>,
    /// Folded entry ids (their subtrees are hidden until unfolded).
    folded: HashSet<String>,
    visible_parent: HashMap<String, Option<String>>,
    visible_children: HashMap<Option<String>, Vec<String>>,
    last_selected_id: Option<String>,
    style: TreeStyle,
}

impl TreeBrowser {
    pub fn new(
        tree: Vec<TreeViewNode>,
        leaf_id: Option<String>,
        max_visible_lines: usize,
        style: TreeStyle,
    ) -> Self {
        let max_visible = max_visible_lines.max(5);
        // Build a pre-order source list + id index. Children retain their
        // source order; `flatten` re-sorts the active branch first.
        let mut src: Vec<SrcEntry> = Vec::new();
        let mut id_to_src: HashMap<String, usize> = HashMap::new();
        fn walk(
            node: &TreeViewNode,
            src: &mut Vec<SrcEntry>,
            id_to_src: &mut HashMap<String, usize>,
        ) {
            // Pre-order: push a placeholder first so `idx` is the node's
            // real source index, then recurse to discover child indices and
            // fill the placeholder's `children`. (Pushing after recursion
            // would make `id_to_src` point at child slots, corrupting
            // parent_of/nearest_visible_index and looping forever.)
            let idx = src.len();
            src.push(SrcEntry {
                id: node.id.clone(),
                parent_id: node.parent_id.clone(),
                message: node.message.clone(),
                label: node.label.clone(),
                label_timestamp: node.label_timestamp.clone(),
                children: Vec::new(),
            });
            id_to_src.insert(node.id.clone(), idx);
            let children: Vec<usize> = node
                .children
                .iter()
                .map(|c| {
                    walk(c, src, id_to_src);
                    *id_to_src.get(&c.id).unwrap()
                })
                .collect();
            src[idx].children = children;
        }
        // Walk roots in source order so flat indices are stable; `flatten`
        // re-orders for the active branch.
        let root_indices: Vec<usize> = tree
            .iter()
            .map(|n| {
                walk(n, &mut src, &mut id_to_src);
                *id_to_src.get(&n.id).unwrap()
            })
            .collect();

        let mut browser = Self {
            src,
            id_to_src,
            flat: Vec::new(),
            filtered: Vec::new(),
            selected: 0,
            leaf_id: leaf_id.clone(),
            max_visible,
            filter_mode: FilterMode::Default,
            search: String::new(),
            multiple_roots: false,
            show_label_timestamps: false,
            active_path: HashSet::new(),
            folded: HashSet::new(),
            visible_parent: HashMap::new(),
            visible_children: HashMap::new(),
            last_selected_id: leaf_id.clone(),
            style,
        };
        browser.flatten(root_indices);
        browser.build_active_path();
        browser.apply_filter();
        browser.selected = browser.nearest_visible_index(browser.leaf_id.as_deref());
        if let Some(id) = browser.selected_id() {
            browser.last_selected_id = Some(id);
        }
        browser
    }

    /// The id at the current cursor, if the filtered list is non-empty.
    pub fn selected_id(&self) -> Option<String> {
        self.filtered
            .get(self.selected)
            .and_then(|&i| self.flat.get(i))
            .map(|n| self.src[n.src].id.clone())
    }

    /// The current search query (for the search line).
    pub fn search_query(&self) -> &str {
        &self.search
    }

    pub fn filter_mode(&self) -> FilterMode {
        self.filter_mode
    }

    pub fn set_filter_mode(&mut self, mode: FilterMode) {
        self.filter_mode = mode;
        self.folded.clear();
        self.apply_filter();
    }

    /// The root→leaf id set, for the active-path marker.
    fn build_active_path(&mut self) {
        self.active_path.clear();
        let Some(leaf) = self.leaf_id.clone() else {
            return;
        };
        let mut current = Some(leaf);
        while let Some(id) = current {
            self.active_path.insert(id.clone());
            current = self.parent_of(&id);
        }
    }

    /// The parent id of `id` in the source tree.
    fn parent_of(&self, id: &str) -> Option<String> {
        self.id_to_src
            .get(id)
            .and_then(|&i| self.src[i].parent_id.clone())
    }

    /// Find the nearest visible ancestor of `entry_id` in the filtered view
    /// (walking up the parent chain), matching pi's `findNearestVisibleIndex`.
    fn nearest_visible_index(&self, entry_id: Option<&str>) -> usize {
        if self.filtered.is_empty() {
            return 0;
        }
        let visible_id_to_index: HashMap<String, usize> = self
            .filtered
            .iter()
            .enumerate()
            .filter_map(|(i, &fi)| self.flat.get(fi).map(|n| (self.src[n.src].id.clone(), i)))
            .collect();
        let mut current = entry_id.map(str::to_owned);
        while let Some(id) = current {
            if let Some(&i) = visible_id_to_index.get(&id) {
                return i;
            }
            current = self.parent_of(&id);
        }
        self.filtered.len() - 1
    }

    /// Flatten the source tree into `flat`, sorting the branch containing
    /// the active leaf first. Faithful port of pi's `flattenTree`
    /// indentation rules. No raw pointers: keys are ids / source indices.
    fn flatten(&mut self, root_indices: Vec<usize>) {
        self.flat.clear();
        // Post-order: which subtrees contain the active leaf.
        let mut contains_active: HashMap<usize, bool> = HashMap::new();
        for i in (0..self.src.len()).rev() {
            let mut has = self.leaf_id.as_deref() == Some(self.src[i].id.as_str());
            for &c in &self.src[i].children {
                if *contains_active.get(&c).unwrap_or(&false) {
                    has = true;
                }
            }
            contains_active.insert(i, has);
        }

        let multiple_roots = root_indices.len() > 1;
        self.multiple_roots = multiple_roots;

        // Order roots: active-branch-containing one first.
        let mut ordered_roots: Vec<usize> = root_indices.clone();
        ordered_roots.sort_by_key(|&i| !contains_active.get(&i).copied().unwrap_or(false));

        #[allow(clippy::type_complexity)]
        let mut stack: Vec<(usize, usize, bool, bool, bool, Vec<GutterInfo>, bool)> = Vec::new();
        for (vis_i, &root) in ordered_roots.iter().enumerate().rev() {
            // Push roots in reverse so they pop forward (active branch first).
            let is_last = vis_i == ordered_roots.len() - 1;
            stack.push((
                root,
                if multiple_roots { 1 } else { 0 },
                multiple_roots,
                multiple_roots,
                is_last,
                Vec::new(),
                multiple_roots,
            ));
        }

        while let Some((
            src_idx,
            indent,
            just_branched,
            show_connector,
            is_last,
            gutters,
            is_vroot,
        )) = stack.pop()
        {
            let flat_index = self.flat.len();
            self.flat.push(FlatNode {
                src: src_idx,
                indent,
                show_connector,
                is_last,
                gutters: gutters.clone(),
                is_virtual_root_child: is_vroot,
            });
            let _ = flat_index;

            let children = &self.src[src_idx].children;
            let multiple_children = children.len() > 1;
            // Sort active-branch children first.
            let mut ordered_children: Vec<usize> = children.clone();
            ordered_children.sort_by_key(|&c| !contains_active.get(&c).copied().unwrap_or(false));

            let child_indent = if multiple_children {
                indent + 1
            } else if just_branched && indent > 0 {
                indent + 1
            } else {
                indent
            };

            let connector_displayed = show_connector && !is_vroot;
            let current_display_indent = if multiple_roots {
                indent.saturating_sub(1)
            } else {
                indent
            };
            let connector_position = current_display_indent.saturating_sub(1);
            let child_gutters = if connector_displayed {
                let mut g = gutters;
                g.push(GutterInfo {
                    position: connector_position,
                    show: !is_last,
                });
                g
            } else {
                gutters
            };

            for (i, &child) in ordered_children.iter().enumerate().rev() {
                // Push in reverse so the stack pops children forward
                // (active-branch-first, matching pi's order); `i == len-1`
                // still correctly labels the last sibling.
                let child_is_last = i == ordered_children.len() - 1;
                stack.push((
                    child,
                    child_indent,
                    multiple_children,
                    multiple_children,
                    child_is_last,
                    child_gutters.clone(),
                    false,
                ));
            }
        }
    }

    /// Re-apply the filter + fold + search to rebuild `filtered`, then
    /// recompute the visible tree's indent/connectors/gutters.
    fn apply_filter(&mut self) {
        if !self.filtered.is_empty() {
            self.last_selected_id = self
                .filtered
                .get(self.selected)
                .and_then(|&i| self.flat.get(i))
                .map(|n| self.src[n.src].id.clone())
                .or(self.last_selected_id.clone());
        }

        let search_tokens: Vec<String> = self
            .search
            .to_lowercase()
            .split_whitespace()
            .map(str::to_owned)
            .collect();

        self.filtered = self
            .flat
            .iter()
            .enumerate()
            .filter_map(|(i, _)| {
                let node = &self.src[self.flat[i].src];
                let is_current_leaf = self.leaf_id.as_deref() == Some(node.id.as_str());

                // Hide assistant messages that are tool-call-only (no text)
                // and not errored/aborted — but always show the active leaf.
                if let TreeMessage::Assistant {
                    text,
                    stop_reason,
                    error_message,
                    ..
                } = &node.message
                    && !is_current_leaf
                {
                    let has_text = text.as_deref().is_some_and(|t| !t.trim().is_empty());
                    let is_error_or_aborted = stop_reason
                        .as_deref()
                        .is_some_and(|s| !is_normal_termination(s))
                        || error_message.is_some();
                    if !has_text && !is_error_or_aborted {
                        return None;
                    }
                }

                let passes = match self.filter_mode {
                    FilterMode::UserOnly => matches!(node.message, TreeMessage::User(_)),
                    FilterMode::NoTools => {
                        !is_settings(node)
                            && !matches!(node.message, TreeMessage::ToolResult { .. })
                    }
                    FilterMode::LabeledOnly => node.label.is_some(),
                    FilterMode::All => true,
                    FilterMode::Default => !is_settings(node),
                };
                if !passes {
                    return None;
                }

                if !search_tokens.is_empty() {
                    let hay = searchable_text(node).to_lowercase();
                    if !search_tokens.iter().all(|t| hay.contains(t)) {
                        return None;
                    }
                }
                Some(i)
            })
            .collect();

        // Hide descendants of folded nodes.
        if !self.folded.is_empty() {
            let mut skip: HashSet<String> = HashSet::new();
            for n in &self.flat {
                let node = &self.src[n.src];
                if let Some(parent) = &node.parent_id
                    && (self.folded.contains(parent) || skip.contains(parent))
                {
                    skip.insert(node.id.clone());
                }
            }
            self.filtered
                .retain(|&i| !skip.contains(&self.src[self.flat[i].src].id));
        }

        self.recalculate_visual_structure();

        if let Some(id) = self.last_selected_id.clone() {
            self.selected = self.nearest_visible_index(Some(&id));
        } else if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
        if !self.filtered.is_empty() {
            self.last_selected_id = self
                .filtered
                .get(self.selected)
                .and_then(|&i| self.flat.get(i))
                .map(|n| self.src[n.src].id.clone());
        }
    }

    /// Recompute indent/connectors/gutters over the *visible* tree: filtering
    /// hides intermediate nodes, so descendants reattach to the nearest
    /// visible ancestor. Faithful port of pi's `recalculateVisualStructure`.
    fn recalculate_visual_structure(&mut self) {
        if self.filtered.is_empty() {
            return;
        }
        let visible_ids: HashSet<String> = self
            .filtered
            .iter()
            .map(|&i| self.src[self.flat[i].src].id.clone())
            .collect();

        let mut visible_parent: HashMap<String, Option<String>> = HashMap::new();
        let mut visible_children: HashMap<Option<String>, Vec<String>> = HashMap::new();
        visible_children.insert(None, Vec::new());

        for &fi in &self.filtered {
            let node = &self.src[self.flat[fi].src];
            // Walk up the source parent chain to the nearest visible ancestor.
            let mut ancestor: Option<String> = None;
            let mut current = node.parent_id.clone();
            while let Some(pid) = current {
                if visible_ids.contains(&pid) {
                    ancestor = Some(pid);
                    break;
                }
                current = self.parent_of(&pid);
            }
            visible_parent.insert(node.id.clone(), ancestor.clone());
            visible_children
                .entry(ancestor)
                .or_default()
                .push(node.id.clone());
        }

        let visible_root_ids = visible_children.get(&None).cloned().unwrap_or_default();
        self.multiple_roots = visible_root_ids.len() > 1;

        #[allow(clippy::type_complexity)]
        let mut stack: Vec<(String, usize, bool, bool, bool, Vec<GutterInfo>, bool)> = Vec::new();
        for (vis_i, root_id) in visible_root_ids.iter().enumerate().rev() {
            // Push visible roots in reverse so they pop forward.
            let is_last = vis_i == visible_root_ids.len() - 1;
            stack.push((
                root_id.clone(),
                if self.multiple_roots { 1 } else { 0 },
                self.multiple_roots,
                self.multiple_roots,
                is_last,
                Vec::new(),
                self.multiple_roots,
            ));
        }
        while let Some((id, indent, just_branched, show_connector, is_last, gutters, is_vroot)) =
            stack.pop()
        {
            // Find the flat entry for this visible id and update its visuals.
            let Some(&fi) = self
                .filtered
                .iter()
                .find(|&&fi| self.src[self.flat[fi].src].id == id)
            else {
                continue;
            };
            let display_indent = if self.multiple_roots {
                indent.saturating_sub(1)
            } else {
                indent
            };
            // Update the FlatNode in place (fi indexes self.flat).
            let node = &mut self.flat[fi];
            node.indent = indent;
            node.show_connector = show_connector;
            node.is_last = is_last;
            node.gutters = gutters.clone();
            node.is_virtual_root_child = is_vroot;

            let children = visible_children
                .get(&Some(id.clone()))
                .cloned()
                .unwrap_or_default();
            let multiple_children = children.len() > 1;
            let child_indent = if multiple_children {
                indent + 1
            } else if just_branched && indent > 0 {
                indent + 1
            } else {
                indent
            };
            let connector_displayed = show_connector && !is_vroot;
            let connector_position = display_indent.saturating_sub(1);
            let child_gutters = if connector_displayed {
                let mut g = gutters;
                g.push(GutterInfo {
                    position: connector_position,
                    show: !is_last,
                });
                g
            } else {
                gutters
            };
            for (i, child) in children.iter().enumerate().rev() {
                // Push in reverse to pop forward (source order).
                let child_is_last = i == children.len() - 1;
                stack.push((
                    child.clone(),
                    child_indent,
                    multiple_children,
                    multiple_children,
                    child_is_last,
                    child_gutters.clone(),
                    false,
                ));
            }
        }

        self.visible_parent = visible_parent;
        self.visible_children = visible_children;
    }

    /// Whether `entry_id` can be folded (has visible children and is a root
    /// or a segment start). Faithful port of pi's `isFoldable`.
    fn is_foldable(&self, entry_id: &str) -> bool {
        let Some(children) = self.visible_children.get(&Some(entry_id.to_owned())) else {
            return false;
        };
        if children.is_empty() {
            return false;
        }
        match self.visible_parent.get(entry_id).cloned().flatten() {
            None => true,
            Some(p) => self
                .visible_children
                .get(&Some(p))
                .map(|s| s.len() > 1)
                .unwrap_or(false),
        }
    }

    /// Find the next branch segment start in `direction` (pi's segment jump).
    fn find_branch_segment_start(&self, direction: SegmentDir) -> usize {
        let Some(selected_id) = self.selected_id() else {
            return self.selected;
        };
        let index_by_id: HashMap<String, usize> = self
            .filtered
            .iter()
            .enumerate()
            .filter_map(|(i, &fi)| self.flat.get(fi).map(|n| (self.src[n.src].id.clone(), i)))
            .collect();
        let mut current = selected_id.clone();
        match direction {
            SegmentDir::Down => loop {
                let children = self
                    .visible_children
                    .get(&Some(current.clone()))
                    .cloned()
                    .unwrap_or_default();
                if children.is_empty() {
                    return index_by_id.get(&current).copied().unwrap_or(self.selected);
                }
                if children.len() > 1 {
                    return index_by_id
                        .get(&children[0])
                        .copied()
                        .unwrap_or(self.selected);
                }
                current = children[0].clone();
            },
            SegmentDir::Up => loop {
                let Some(parent) = self.visible_parent.get(&current).cloned().flatten() else {
                    return index_by_id.get(&current).copied().unwrap_or(self.selected);
                };
                let children = self
                    .visible_children
                    .get(&Some(parent.clone()))
                    .cloned()
                    .unwrap_or_default();
                if children.len() > 1
                    && let Some(&seg) = index_by_id.get(&current)
                    && seg < self.selected
                {
                    return seg;
                }
                current = parent;
            },
        }
    }

    /// The rendered lines for the current viewport, including the
    /// `(N/M)` footer and filter-status suffix. The host prepends its own
    /// title/help/search lines.
    pub fn render(&self, width: usize) -> Vec<String> {
        let mut lines = Vec::new();
        let w = width.max(1);
        let accent = self.style.accent;
        let muted = self.style.muted;
        let dim = self.style.muted;
        let sel_bg = self.style.selected_bg;

        if self.filtered.is_empty() {
            lines.push(truncate_to_cells(
                &format!("{muted}  No entries found{RESET}"),
                w,
            ));
            lines.push(truncate_to_cells(
                &format!(
                    "{muted}  (0/0){}{RESET}",
                    format!(
                        "{}{}",
                        self.filter_mode.label(),
                        label_times_suffix(self.show_label_timestamps)
                    )
                ),
                w,
            ));
            return lines;
        }

        let start = ((self.selected as isize - self.max_visible as isize / 2).max(0) as usize)
            .min(self.filtered.len().saturating_sub(self.max_visible));
        let end = (start + self.max_visible).min(self.filtered.len());

        for i in start..end {
            let &fi = self.filtered.get(i).unwrap();
            let flat = &self.flat[fi];
            let node = &self.src[flat.src];
            let is_selected = i == self.selected;

            let cursor = if is_selected {
                format!("{accent}› {RESET}")
            } else {
                "  ".to_owned()
            };

            let display_indent = if self.multiple_roots {
                flat.indent.saturating_sub(1)
            } else {
                flat.indent
            };
            let connector = if flat.show_connector && !flat.is_virtual_root_child {
                if flat.is_last { "└─ " } else { "├─ " }
            } else {
                ""
            };
            let connector_position = if connector.is_empty() {
                usize::MAX
            } else {
                display_indent.saturating_sub(1)
            };
            let is_folded = self.folded.contains(&node.id);

            // Build the prefix char-by-char, placing gutters + connector.
            let total_chars = display_indent * 3;
            let mut prefix = String::with_capacity(total_chars);
            for c in 0..total_chars {
                let level = c / 3;
                let pos_in_level = c % 3;
                let gutter = flat.gutters.iter().find(|g| g.position == level);
                if let Some(g) = gutter {
                    if pos_in_level == 0 {
                        prefix.push(if g.show { '│' } else { ' ' });
                    } else {
                        prefix.push(' ');
                    }
                } else if !connector.is_empty() && level == connector_position {
                    if pos_in_level == 0 {
                        prefix.push(if flat.is_last { '└' } else { '├' });
                    } else if pos_in_level == 1 {
                        let foldable = self.is_foldable(&node.id);
                        prefix.push(if is_folded {
                            '⊞'
                        } else if foldable {
                            '⊟'
                        } else {
                            '─'
                        });
                    } else {
                        prefix.push(' ');
                    }
                } else {
                    prefix.push(' ');
                }
            }

            let shows_fold_in_connector = flat.show_connector && !flat.is_virtual_root_child;
            let fold_marker = if is_folded && !shows_fold_in_connector {
                format!("{accent}⊞ {RESET}")
            } else {
                String::new()
            };

            let is_on_active_path = self.active_path.contains(&node.id);
            let path_marker = if is_on_active_path {
                format!("{accent}• {RESET}")
            } else {
                String::new()
            };

            let label_part = node
                .label
                .as_deref()
                .map(|l| format!("{}[{}] {}", self.style.warning, l, RESET));
            let label_timestamp = if self.show_label_timestamps
                && node.label.is_some()
                && node.label_timestamp.is_some()
            {
                format!(
                    "{} {}",
                    format_label_timestamp(node.label_timestamp.as_deref().unwrap_or("")),
                    RESET
                )
            } else {
                String::new()
            };

            let content = self.entry_display_text(node, is_selected);

            let mut line = format!(
                "{cursor}{dim}{prefix}{RESET}{fold_marker}{path_marker}{label_part}{label_timestamp}{content}",
                label_part = label_part.unwrap_or_default(),
            );
            if is_selected {
                line = format!("{sel_bg}{line}{RESET}");
            }
            lines.push(truncate_to_cells(&line, w));
        }

        let footer = format!(
            "{muted}  ({}/{}){}{}{RESET}",
            self.selected + 1,
            self.filtered.len(),
            self.filter_mode.label(),
            label_times_suffix(self.show_label_timestamps),
        );
        lines.push(truncate_to_cells(&footer, w));
        let _ = (accent, dim, sel_bg, node_unused());
        lines
    }

    /// One node's display text, pi-style: `user: …`, `assistant: …`,
    /// `[read: path]`, etc. Bolded when selected.
    fn entry_display_text(&self, node: &SrcEntry, is_selected: bool) -> String {
        let result = match &node.message {
            TreeMessage::User(text) => {
                format!("{}user: {}{}", self.style.accent, RESET, normalize(text))
            }
            TreeMessage::Assistant {
                text,
                stop_reason,
                error_message,
                ..
            } => {
                let head = format!("{}assistant: {}", self.style.success, RESET);
                let body = match text {
                    Some(t) if !t.trim().is_empty() => normalize(t),
                    _ => {
                        if stop_reason.as_deref() == Some("aborted") {
                            format!("{}(aborted){}", self.style.muted, RESET)
                        } else if let Some(err) = error_message {
                            let e: String = normalize(err).chars().take(80).collect();
                            format!("{}{e}{}", self.style.error, RESET)
                        } else {
                            format!("{}(no content){}", self.style.muted, RESET)
                        }
                    }
                };
                format!("{head}{body}")
            }
            TreeMessage::ToolResult {
                tool_call_id,
                tool_name,
            } => {
                if let Some(tc_id) = tool_call_id
                    && let Some((_, name, args)) = self.tool_call_lookup(tc_id)
                {
                    format!(
                        "{}{}{}",
                        self.style.muted,
                        format_tool_call(&name, &args),
                        RESET
                    )
                } else {
                    format!(
                        "{}[{}]{}",
                        self.style.muted,
                        tool_name.as_deref().unwrap_or("tool"),
                        RESET
                    )
                }
            }
        };
        if is_selected {
            format!("{BOLD}{result}{RESET}")
        } else {
            result
        }
    }

    /// Scan the source tree for the assistant turn that issued `tool_call_id`.
    fn tool_call_lookup(&self, tool_call_id: &str) -> Option<(String, String, Value)> {
        for entry in &self.src {
            if let TreeMessage::Assistant { tool_calls, .. } = &entry.message {
                for (tc_id, name, args) in tool_calls {
                    if tc_id == tool_call_id {
                        return Some((tc_id.clone(), name.clone(), args.clone()));
                    }
                }
            }
        }
        None
    }

    /// Dispatch a key. Mirrors pi's `handleInput`: arrows, page, fold/unfold
    /// (+ segment jump), the five filters + cycling, search typing, label
    /// editing, and the timestamp toggle.
    pub fn handle_key(&mut self, key: KeyEvent) -> TreeKey {
        let ctrl = key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match key.code {
            KeyCode::Up if !ctrl => {
                self.selected = if self.selected == 0 {
                    self.filtered.len().saturating_sub(1)
                } else {
                    self.selected - 1
                };
            }
            KeyCode::Down if !ctrl => {
                let len = self.filtered.len();
                self.selected = if self.selected + 1 >= len {
                    0
                } else {
                    self.selected + 1
                };
            }
            // Ctrl+Left / Alt+Left: fold-or-up (pi's app.tree.foldOrUp).
            KeyCode::Left if ctrl => return self.fold_or_up(),
            // Ctrl+Right / Alt+Right: unfold-or-down.
            KeyCode::Right if ctrl => return self.unfold_or_down(),
            KeyCode::PageUp => {
                self.selected = self.selected.saturating_sub(self.max_visible);
            }
            KeyCode::PageDown => {
                self.selected =
                    (self.selected + self.max_visible).min(self.filtered.len().saturating_sub(1));
            }
            KeyCode::Enter => {
                if let Some(id) = self.selected_id() {
                    return TreeKey::Handled(Some(TreeAction::Navigate(id)));
                }
            }
            KeyCode::Esc => {
                if !self.search.is_empty() {
                    self.search.clear();
                    self.folded.clear();
                    self.apply_filter();
                } else {
                    return TreeKey::Handled(Some(TreeAction::Cancel));
                }
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.set_filter_mode(FilterMode::Default);
            }
            KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.toggle_filter(FilterMode::NoTools);
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.toggle_filter(FilterMode::UserOnly);
            }
            KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.toggle_filter(FilterMode::LabeledOnly);
            }
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.toggle_filter(FilterMode::All);
            }
            KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let cycle = if key.modifiers.contains(KeyModifiers::SHIFT) {
                    self.filter_mode.prev()
                } else {
                    self.filter_mode.next()
                };
                self.set_filter_mode(cycle);
            }
            KeyCode::Backspace => {
                if !self.search.is_empty() {
                    self.search.pop();
                    self.folded.clear();
                    self.apply_filter();
                }
            }
            // Shift+L: edit the selected node's label (host opens editor).
            KeyCode::Char('L') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                if let Some(id) = self.selected_id() {
                    return TreeKey::Handled(Some(TreeAction::Label {
                        entry_id: id,
                        label: self.current_label_for_action(),
                    }));
                }
            }
            KeyCode::Char('T') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.show_label_timestamps = !self.show_label_timestamps;
            }
            KeyCode::Char(ch)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT)
                    && !is_control_char(ch) =>
            {
                self.search.push(ch);
                self.folded.clear();
                self.apply_filter();
            }
            _ => return TreeKey::Unhandled,
        }
        TreeKey::Handled(None)
    }

    /// The selected node's current label, for the `Label` action seed.
    fn current_label_for_action(&self) -> Option<String> {
        self.filtered
            .get(self.selected)
            .and_then(|&i| self.flat.get(i))
            .and_then(|n| self.src[n.src].label.clone())
    }

    fn toggle_filter(&mut self, mode: FilterMode) {
        self.filter_mode = if self.filter_mode == mode {
            FilterMode::Default
        } else {
            mode
        };
        self.folded.clear();
        self.apply_filter();
    }

    fn fold_or_up(&mut self) -> TreeKey {
        let Some(id) = self.selected_id() else {
            return TreeKey::Handled(None);
        };
        if self.is_foldable(&id) && !self.folded.contains(&id) {
            self.folded.insert(id);
            self.apply_filter();
        } else {
            self.selected = self.find_branch_segment_start(SegmentDir::Up);
        }
        TreeKey::Handled(None)
    }

    fn unfold_or_down(&mut self) -> TreeKey {
        let Some(id) = self.selected_id() else {
            return TreeKey::Handled(None);
        };
        if self.folded.contains(&id) {
            self.folded.remove(&id);
            self.apply_filter();
        } else {
            self.selected = self.find_branch_segment_start(SegmentDir::Down);
        }
        TreeKey::Handled(None)
    }
}

#[derive(Clone, Copy)]
enum SegmentDir {
    Up,
    Down,
}

fn label_times_suffix(on: bool) -> &'static str {
    if on { " [+label time]" } else { "" }
}

fn node_unused() -> usize {
    0
}

/// Whether a serialized `StopReason` string represents normal completion
/// (not an error/abort). Tolerant of pi's camelCase (`toolUse`) and phi's
/// `#[serde(rename_all = "snake_case")]` form (`tool_use`) so the widget
/// adapts to whichever the host serialized.
fn is_normal_termination(s: &str) -> bool {
    s == "stop" || s == "toolUse" || s == "tool_use"
}

fn is_settings(_node: &SrcEntry) -> bool {
    // phi has no model_change/thinking_level_change/session_info entries in
    // the tree yet (those land with RLM/compaction). The browser treats
    // only labels specially, and labels are rendered inline, so nothing is
    // "settings" today. Kept so the filter semantics match pi's exactly
    // when those entry types arrive.
    false
}

fn is_control_char(ch: char) -> bool {
    let code = ch as u32;
    code < 32 || code == 0x7f || (0x80..=0x9f).contains(&code)
}

fn normalize(s: &str) -> String {
    s.replace(['\n', '\t'], " ").trim().to_owned()
}

fn searchable_text(node: &SrcEntry) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(l) = &node.label {
        parts.push(l.clone());
    }
    match &node.message {
        TreeMessage::User(t) => parts.push(format!("user {}", t)),
        TreeMessage::Assistant {
            text,
            stop_reason,
            error_message,
            tool_calls,
        } => {
            parts.push("assistant".to_owned());
            if let Some(t) = text {
                parts.push(t.clone());
            }
            for (_id, name, args) in tool_calls {
                parts.push(name.clone());
                parts.push(args.to_string());
            }
            if let Some(s) = stop_reason {
                parts.push(s.clone());
            }
            if let Some(e) = error_message {
                parts.push(e.clone());
            }
        }
        TreeMessage::ToolResult { tool_name, .. } => {
            parts.push(tool_name.clone().unwrap_or_else(|| "tool".to_owned()));
        }
    }
    parts.join(" ")
}

/// Format a tool call as pi does: `[read: path]`, `[bash: cmd]`, etc.
fn format_tool_call(name: &str, args: &Value) -> String {
    let shorten = |p: &str| -> String {
        if let Some(home) = std::env::var_os("HOME") {
            let home = home.to_string_lossy();
            if let Some(rest) = p.strip_prefix(home.as_ref()) {
                return format!("~{rest}");
            }
        }
        p.to_owned()
    };
    match name {
        "read" => {
            let path = args
                .get("path")
                .or_else(|| args.get("file_path"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let path = shorten(path);
            let offset = args.get("offset").and_then(Value::as_u64);
            let limit = args.get("limit").and_then(Value::as_u64);
            let display = if offset.is_some() || limit.is_some() {
                let start = offset.unwrap_or(1);
                let end = limit.map(|l| start + l - 1);
                format!(
                    "{path}:{start}{}",
                    end.map(|e| format!("-{e}")).unwrap_or_default()
                )
            } else {
                path
            };
            format!("[read: {display}]")
        }
        "write" => {
            let path = args
                .get("path")
                .or_else(|| args.get("file_path"))
                .and_then(Value::as_str)
                .unwrap_or("");
            format!("[write: {}]", shorten(path))
        }
        "edit" => {
            let path = args
                .get("path")
                .or_else(|| args.get("file_path"))
                .and_then(Value::as_str)
                .unwrap_or("");
            format!("[edit: {}]", shorten(path))
        }
        "bash" => {
            let raw = args.get("command").and_then(Value::as_str).unwrap_or("");
            let cmd: String = raw
                .replace(['\n', '\t'], " ")
                .trim()
                .chars()
                .take(50)
                .collect();
            let suffix = if raw.chars().count() > 50 { "..." } else { "" };
            format!("[bash: {cmd}{suffix}]")
        }
        "grep" => {
            let pattern = args.get("pattern").and_then(Value::as_str).unwrap_or("");
            let path = shorten(args.get("path").and_then(Value::as_str).unwrap_or("."));
            format!("[grep: /{pattern}/ in {path}]")
        }
        "find" => {
            let pattern = args.get("pattern").and_then(Value::as_str).unwrap_or("");
            let path = shorten(args.get("path").and_then(Value::as_str).unwrap_or("."));
            format!("[find: {pattern} in {path}]")
        }
        "ls" => {
            let path = shorten(args.get("path").and_then(Value::as_str).unwrap_or("."));
            format!("[ls: {path}]")
        }
        _ => {
            let s = serde_json::to_string(args).unwrap_or_default();
            let truncated: String = s.chars().take(40).collect();
            let suffix = if s.chars().count() > 40 { "..." } else { "" };
            format!("[{name}: {truncated}{suffix}]")
        }
    }
}

/// Compact HH:MM (today), M/D HH:MM (this year), or YY/M/D HH:MM.
fn format_label_timestamp(ts: &str) -> String {
    let date = parse_ts(ts);
    let Some(d) = date else {
        return ts.to_owned();
    };
    let now = now_ts();
    let time = format!("{:02}:{:02}", d.hour, d.min);
    if d.year == now.year && d.month == now.month && d.day == now.day {
        return time;
    }
    if d.year == now.year {
        return format!("{}/{} {time}", d.month, d.day);
    }
    let yr = d.year % 100;
    format!("{yr:02}/{}/{} {}", d.month, d.day, time)
}

struct DateParts {
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    min: u32,
}

fn parse_ts(ts: &str) -> Option<DateParts> {
    let ts = ts.trim();
    if ts.len() < 16 {
        return None;
    }
    let year: i32 = ts.get(0..4)?.parse().ok()?;
    let month: u32 = ts.get(5..7)?.parse().ok()?;
    let day: u32 = ts.get(8..10)?.parse().ok()?;
    let hour: u32 = ts.get(11..13)?.parse().ok()?;
    let min: u32 = ts.get(14..16)?.parse().ok()?;
    Some(DateParts {
        year,
        month,
        day,
        hour,
        min,
    })
}

fn now_ts() -> DateParts {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    civil(secs)
}

/// UTC civil date parts from a Unix timestamp (sufficient for date labels).
fn civil(unix_secs: u64) -> DateParts {
    let days = (unix_secs / 86_400) as i64;
    let secs_of_day = unix_secs % 86_400;
    let hour = (secs_of_day / 3600) as u32;
    let min = ((secs_of_day % 3600) / 60) as u32;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = (y + if m <= 2 { 1 } else { 0 }) as i32;
    DateParts {
        year,
        month: m,
        day: d,
        hour,
        min,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;

    fn user(id: &str, parent: Option<&str>, text: &str) -> TreeViewNode {
        TreeViewNode {
            id: id.to_owned(),
            parent_id: parent.map(str::to_owned),
            message: TreeMessage::User(text.to_owned()),
            label: None,
            label_timestamp: None,
            children: Vec::new(),
        }
    }

    fn assistant(id: &str, parent: Option<&str>, text: &str) -> TreeViewNode {
        TreeViewNode {
            id: id.to_owned(),
            parent_id: parent.map(str::to_owned),
            message: TreeMessage::Assistant {
                text: Some(text.to_owned()),
                tool_calls: Vec::new(),
                stop_reason: Some("stop".to_owned()),
                error_message: None,
            },
            label: None,
            label_timestamp: None,
            children: Vec::new(),
        }
    }

    fn toolcall_only(id: &str, parent: Option<&str>) -> TreeViewNode {
        TreeViewNode {
            id: id.to_owned(),
            parent_id: parent.map(str::to_owned),
            message: TreeMessage::Assistant {
                text: None,
                tool_calls: vec![(
                    "tc-1".to_owned(),
                    "read".to_owned(),
                    serde_json::json!({"path":"x.ts"}),
                )],
                stop_reason: Some("tool_use".to_owned()),
                error_message: None,
            },
            label: None,
            label_timestamp: None,
            children: Vec::new(),
        }
    }

    /// One-shot builder: convert a flat list into the tree shape.
    fn build(nodes: &[TreeViewNode]) -> Vec<TreeViewNode> {
        let mut by_id: HashMap<String, TreeViewNode> =
            nodes.iter().cloned().map(|n| (n.id.clone(), n)).collect();
        let mut children_of: HashMap<String, Vec<String>> = HashMap::new();
        let mut root_ids: Vec<String> = Vec::new();
        for n in nodes {
            match &n.parent_id {
                Some(p) if by_id.contains_key(p) => {
                    children_of.entry(p.clone()).or_default().push(n.id.clone());
                }
                _ => root_ids.push(n.id.clone()),
            }
        }
        fn build_rec(
            id: &str,
            by_id: &mut HashMap<String, TreeViewNode>,
            children_of: &HashMap<String, Vec<String>>,
        ) -> TreeViewNode {
            let mut node = by_id.remove(id).unwrap();
            if let Some(kids) = children_of.get(id) {
                for k in kids {
                    node.children.push(build_rec(k, by_id, children_of));
                }
            }
            node
        }
        root_ids
            .into_iter()
            .map(|id| build_rec(&id, &mut by_id, &children_of))
            .collect()
    }

    fn event(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn event_ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    #[test]
    fn renders_a_linear_conversation_with_connectors() {
        let nodes = [
            user("u1", None, "hello"),
            assistant("a1", Some("u1"), "hi"),
            user("u2", Some("a1"), "again"),
        ];
        let tree = build(&nodes);
        let browser = TreeBrowser::new(tree, Some("u2".to_owned()), 24, TreeStyle::default());
        let lines = browser.render(80);
        assert!(lines.len() >= 4);
        assert!(
            lines.iter().any(|l| l.contains('•')),
            "active-path dot shown"
        );
        assert!(lines.iter().any(|l| l.contains("user:")));
        assert!(lines.iter().any(|l| l.contains("assistant:")));
    }

    #[test]
    fn folding_a_subtree_hides_its_descendants() {
        // Mirrors pi's `buildBranchingTree`: the foldable node is the
        // *segment start* (a branch point's child with descendants), not
        // the branch point itself — pi's `isFoldable` requires the node's
        // visible parent to have >1 child. Here `u3a` is the segment start
        // (visible child `a3a`; parent `a1` has visible children `[u3a, u3b]`).
        let nodes = [
            user("u1", None, "root"),
            assistant("a1", Some("u1"), "reply"),
            user("u3a", Some("a1"), "branch A"),
            assistant("a3a", Some("u3a"), "A reply"),
            user("u3b", Some("a1"), "branch B"),
        ];
        let tree = build(&nodes);
        let mut browser = TreeBrowser::new(tree, Some("a3a".to_owned()), 24, TreeStyle::default());

        // Cursor starts on the active leaf (a3a); move up to the segment
        // start u3a, then fold it.
        while browser.selected_id().as_deref() != Some("u3a") {
            let _ = browser.handle_key(event(KeyCode::Up));
        }
        assert_eq!(browser.selected_id().as_deref(), Some("u3a"));
        let _ = browser.handle_key(event_ctrl(KeyCode::Left));
        let lines = browser.render(80).join("\n");
        // The folded node itself stays visible (pi folds only descendants);
        // its subtree (`a3a`) is hidden. The sibling branch and root remain.
        assert!(
            lines.contains("branch A"),
            "folded node label stays visible"
        );
        assert!(!lines.contains("A reply"), "folded descendant hidden");
        assert!(lines.contains("root"));
        assert!(lines.contains("branch B"), "sibling branch unaffected");
    }

    #[test]
    fn ctrl_u_filter_jumps_cursor_to_nearest_user_ancestor() {
        let nodes = [
            user("u1", None, "first"),
            assistant("a1", Some("u1"), "reply 1"),
            user("u2", Some("a1"), "second"),
            assistant("a2", Some("u2"), "reply 2"),
        ];
        let tree = build(&nodes);
        let mut browser = TreeBrowser::new(tree, Some("a2".to_owned()), 24, TreeStyle::default());
        assert_eq!(browser.selected_id().as_deref(), Some("a2"));
        let _ = browser.handle_key(event_ctrl(KeyCode::Char('u')));
        assert_eq!(browser.selected_id().as_deref(), Some("u2"));
    }

    #[test]
    fn toolcall_only_assistant_hidden_in_default_filter() {
        let nodes = [
            user("u1", None, "prompt"),
            toolcall_only("a1", Some("u1")),
            user("u2", Some("a1"), "follow up"),
        ];
        let tree = build(&nodes);
        let browser = TreeBrowser::new(tree, Some("u2".to_owned()), 24, TreeStyle::default());
        let body = browser.render(80).join("\n");
        assert!(!body.contains("no content"));
    }

    #[test]
    fn enter_emits_navigate_action_for_selected_entry() {
        let nodes = [
            user("u1", None, "prompt"),
            assistant("a1", Some("u1"), "reply"),
        ];
        let tree = build(&nodes);
        let mut browser = TreeBrowser::new(tree, Some("a1".to_owned()), 24, TreeStyle::default());
        // Move to u1, then Enter should navigate there.
        let _ = browser.handle_key(event(KeyCode::Up));
        match browser.handle_key(event(KeyCode::Enter)) {
            TreeKey::Handled(Some(TreeAction::Navigate(id))) => assert_eq!(id, "u1"),
            other => panic!(
                "expected Navigate, got {other:?}",
                other = match other {
                    _ => other,
                }
            ),
        }
    }

    #[test]
    fn escape_cancels_when_search_is_empty() {
        let nodes = [user("u1", None, "prompt")];
        let tree = build(&nodes);
        let mut browser = TreeBrowser::new(tree, Some("u1".to_owned()), 24, TreeStyle::default());
        match browser.handle_key(event(KeyCode::Esc)) {
            TreeKey::Handled(Some(TreeAction::Cancel)) => {}
            other => panic!("expected Cancel, got {other:?}"),
        }
    }

    #[test]
    fn escape_clears_search_instead_of_cancelling() {
        let nodes = [user("u1", None, "prompt")];
        let tree = build(&nodes);
        let mut browser = TreeBrowser::new(tree, Some("u1".to_owned()), 24, TreeStyle::default());
        // Type into search, then Esc clears (not cancel).
        let _ = browser.handle_key(event(KeyCode::Char('x')));
        assert_eq!(browser.search_query(), "x");
        match browser.handle_key(event(KeyCode::Esc)) {
            TreeKey::Handled(None) => {}
            other => panic!("expected Handled(None) (clear), got {other:?}"),
        }
        assert_eq!(browser.search_query(), "");
    }

    /// Regression: the source-list `walk` once pushed the parent *after*
    /// recursing into children (post-order), so `id_to_src` mapped every
    /// node to a child's index. That corrupted `parent_of` into returning
    /// the node itself, making `nearest_visible_index` loop forever and the
    /// constructor hang on any multi-node tree. Verifies the parent chain is
    /// well-formed (leaf → root terminates) and the active leaf resolves.
    #[test]
    fn constructor_does_not_hang_on_a_multi_node_tree() {
        let nodes = [
            user("u1", None, "hello"),
            assistant("a1", Some("u1"), "hi"),
            user("u2", Some("a1"), "again"),
            assistant("a2", Some("u2"), "reply"),
        ];
        let tree = build(&nodes);
        // Would hang under the post-order bug; selection lands on the leaf.
        let browser = TreeBrowser::new(tree, Some("a2".to_owned()), 24, TreeStyle::default());
        assert_eq!(browser.selected_id().as_deref(), Some("a2"));
    }

    /// Regression: pi pushes siblings in reverse onto a stack so they pop
    /// forward (active-branch first). The Rust port once pushed forward,
    /// reversing sibling order and mislabeling `is_last` so the active
    /// branch got the `└─` (last) connector and appeared second.
    #[test]
    fn active_branch_sorts_first_among_siblings() {
        let nodes = [
            user("u1", None, "root"),
            assistant("a1", Some("u1"), "reply"),
            user("u3a", Some("a1"), "branch A (active)"),
            assistant("a3a", Some("u3a"), "A reply"),
            user("u3b", Some("a1"), "branch B"),
        ];
        let tree = build(&nodes);
        let browser = TreeBrowser::new(tree, Some("a3a".to_owned()), 24, TreeStyle::default());
        let body = browser.render(80).join("\n");
        let a_pos = body.find("branch A (active)").unwrap_or(usize::MAX);
        let b_pos = body.find("branch B").unwrap_or(usize::MAX);
        assert!(a_pos < b_pos, "active branch renders before its sibling");
    }
}
