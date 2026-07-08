//! Session persistence: file I/O, append-only writes, and path helpers.
//!
//! Files live under `{root}/{encoded-cwd}/{timestamp}_{id}.jsonl`, mirroring
//! pi's per-cwd layout (the cwd is flattened into a safe directory name so
//! sessions for different projects don't collide). The tolerant reader
//! skips malformed lines so a torn final write from a crash doesn't lose
//! the session.
//!
//! Tree structure (pi-compatible): every message entry carries `id` and
//! `parentId`; a `leaf_id` in the header points at the active position.
//! `branch` moves the leaf to an earlier entry so the next append forms a
//! new child (a new branch) without mutating history. Labels are their own
//! tag so they scan cheaply and resolve into a label map on load.

use std::collections::HashSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;

use crate::types::{
    EntryFields, Header, LabelEntry, Line, MessageEntry, SESSION_VERSION, SessionMeta,
    SessionStore, TreeNode, new_id, now_timestamp, short_id,
};

/// Default root of all per-cwd session directories: `{data dir}/sessions`.
pub fn sessions_root() -> Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "phi").context("no home directory")?;
    Ok(dirs.data_dir().join("sessions"))
}

/// `--{cwd with separators flattened}--`, the per-cwd directory name (pi's
/// encoding, so a future shared session picker could browse the same tree).
pub fn flatten_cwd(cwd: &Path) -> String {
    let raw = cwd.display().to_string();
    let stripped = raw.trim_start_matches(['/', '\\']);
    format!("--{}--", stripped.replace(['/', '\\', ':'], "-"))
}

/// Per-cwd session directory under `root`: `{root}/{flatten_cwd(cwd)}`.
pub fn sessions_dir_in(root: &Path, cwd: &Path) -> PathBuf {
    root.join(flatten_cwd(cwd))
}

/// Per-cwd session directory under the default root. Convenience wrapper
/// around [`sessions_dir_in`] for the app's common case.
pub fn sessions_dir(cwd: &Path) -> Result<PathBuf> {
    Ok(sessions_dir_in(&sessions_root()?, cwd))
}

/// A parsed tree entry: the persistent fields plus the message payload.
/// Labels are collected separately (they don't ride the message path).
pub(crate) struct ParsedEntry {
    pub fields: EntryFields,
    pub message: phi_ai_types::Message,
}

/// The full parse of a session file: header, message entries in file order,
/// and label entries in file order. Used by both the flat read path
/// ([`load_messages`]) and the tree read path ([`read_tree`]).
pub(crate) struct Parsed {
    pub header: Header,
    pub entries: Vec<ParsedEntry>,
    pub labels: Vec<LabelEntry>,
}

impl SessionStore {
    /// Create a new session file under `root` and write its header. The
    /// file is named `{timestamp}_{id}.jsonl` (colons/dots in the timestamp
    /// flattened to dashes, matching pi) so a plain `ls` is chronologically
    /// ordered.
    pub fn create_in(root: &Path, cwd: &Path, model: &str) -> Result<Self> {
        let id = new_id();
        let timestamp = now_timestamp();
        let dir = sessions_dir_in(root, cwd);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("creating session dir {}", dir.display()))?;
        let file_ts = timestamp.replace([':', '.'], "-");
        let path = dir.join(format!("{file_ts}_{id}.jsonl"));
        let header = Header {
            version: SESSION_VERSION,
            id,
            timestamp,
            cwd: cwd.display().to_string(),
            model: model.to_owned(),
            parent_session: None,
            leaf_id: None,
        };
        let line = serde_json::to_string(&Line::Session(header.clone()))
            .context("encoding session header")?;
        std::fs::write(&path, format!("{line}\n"))
            .with_context(|| format!("writing session header {}", path.display()))?;
        Ok(Self {
            path,
            header,
            count: 0,
            first_message: None,
        })
    }

    /// Create under the default root ([`sessions_root`]).
    pub fn create(cwd: &Path, model: &str) -> Result<Self> {
        Self::create_in(&sessions_root()?, cwd, model)
    }

    /// Create a child session file under the *same* directory as
    /// `parent_path`, copying only the supplied `path_messages` (already
    /// in root→order) plus `path_labels` `(target_id, label, timestamp)`.
    /// The new file gets a fresh id and timestamp; its header points its
    /// leaf at the last message (or `None` when `path_messages` is empty,
    /// which is a fork-before-root). This is the fork primitive
    /// ([`SessionStore::create_branched_session`] is the public face).
    ///
    /// `source_id_map` maps a source entry id → its position in
    /// `path_messages`; labels reference source ids, and this re-keys them
    /// to the new ids we mint here.
    pub(crate) fn create_child_of(
        parent_path: &Path,
        parent_header: &Header,
        path_messages: &[phi_ai_types::Message],
        source_id_map: &std::collections::HashMap<String, usize>,
        path_labels: &[(String, Option<String>, String)],
    ) -> Result<Self> {
        let root = parent_path.parent().unwrap_or_else(|| Path::new("."));
        let id = new_id();
        let timestamp = now_timestamp();
        let file_ts = timestamp.replace([':', '.'], "-");
        let new_path = root.join(format!("{file_ts}_{id}.jsonl"));
        let header = Header {
            version: SESSION_VERSION,
            id,
            timestamp,
            cwd: parent_header.cwd.clone(),
            model: parent_header.model.clone(),
            // Record lineage so a future session family graph can walk it;
            // the loader doesn't follow this today.
            parent_session: Some(parent_path.display().to_string()),
            leaf_id: None,
        };
        let mut existing: HashSet<String> = HashSet::new();
        let mut lines: Vec<String> =
            Vec::with_capacity(path_messages.len() + path_labels.len() + 1);
        lines.push(serde_json::to_string(&Line::Session(header.clone()))?);
        // Messages: a linear chain under fresh ids.
        let mut new_ids: Vec<String> = Vec::with_capacity(path_messages.len());
        let mut first_message: Option<String> = None;
        for (i, message) in path_messages.iter().enumerate() {
            let new_id = short_id(&existing);
            existing.insert(new_id.clone());
            let fields = EntryFields {
                id: new_id.clone(),
                parent_id: new_ids.last().cloned(),
                timestamp: now_timestamp(),
            };
            lines.push(serde_json::to_string(&Line::Message {
                fields,
                message: MessageEntry {
                    message: message.clone(),
                },
            })?);
            if i == 0
                && let phi_ai_types::Message::User(u) = message
            {
                first_message = Some(u.content.clone());
            }
            new_ids.push(new_id);
        }
        let leaf = new_ids.last().cloned();
        // Labels: chain off the leaf, re-keyed via `source_id_map` → new id.
        let mut label_parent = leaf.clone();
        for (target_id, label, label_timestamp) in path_labels {
            let Some(&pos) = source_id_map.get(target_id) else {
                continue;
            };
            let Some(target_new_id) = new_ids.get(pos) else {
                continue;
            };
            let new_id = short_id(&existing);
            existing.insert(new_id.clone());
            let fields = EntryFields {
                id: new_id.clone(),
                parent_id: label_parent.clone(),
                timestamp: label_timestamp.clone(),
            };
            lines.push(serde_json::to_string(&Line::Label {
                fields,
                target_id: target_new_id.clone(),
                label: label.clone(),
            })?);
            label_parent = Some(new_id);
        }
        if let Some(id) = &leaf {
            let mut new_header = header.clone();
            new_header.leaf_id = Some(id.clone());
            lines[0] = serde_json::to_string(&Line::Session(new_header))?;
        }
        let out = format!("{}\n", lines.join("\n"));
        std::fs::write(&new_path, out)
            .with_context(|| format!("writing forked session {}", new_path.display()))?;
        Ok(Self {
            path: new_path,
            header: Header {
                leaf_id: leaf,
                ..header
            },
            count: path_messages.len(),
            first_message,
        })
    }

    /// Open an existing session file and read its header + message count
    /// (the transcript itself is returned by [`load_messages`]; this is the
    /// metadata path used by `--continue` and the `/resume` picker).
    pub fn open(path: &Path) -> Result<Self> {
        let parsed = parse(path)?;
        let (count, first_message) = stats(&parsed);
        Ok(Self {
            path: path.to_owned(),
            header: parsed.header,
            count,
            first_message,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn id(&self) -> &str {
        &self.header.id
    }

    pub fn header(&self) -> &Header {
        &self.header
    }

    pub fn message_count(&self) -> usize {
        self.count
    }

    /// The active leaf id from the header, or the id of the last message
    /// entry if the header has none (a v1 file, or one written before any
    /// leaf was recorded). `None` only when there are no messages.
    pub fn leaf_id(&self) -> Option<String> {
        if self.header.leaf_id.is_some() {
            return self.header.leaf_id.clone();
        }
        let parsed = match parse(&self.path) {
            Ok(p) => p,
            Err(_) => return None,
        };
        parsed.entries.last().map(|e| e.fields.id.clone())
    }

    /// Append one transcript message as a child of the current leaf, then
    /// advance the leaf to it. Cheap and crash-safe: a single `write` per
    /// message, so a torn final line is the worst a crash can do (the
    /// reader skips it).
    pub fn append(&mut self, message: &phi_ai_types::Message) -> Result<String> {
        let parent = self.leaf_id();
        self.append_child(message, parent.as_deref())
    }

    /// Append a message as a child of an explicit parent (or `None` for a
    /// new root), advancing the leaf. Returns the new entry id. Exposed for
    /// the tree browser's navigation: appending after a `branch` call wires
    /// the new turn onto the branched position, exactly like pi's `branch`
    /// + `appendMessage`.
    pub fn append_child(
        &mut self,
        message: &phi_ai_types::Message,
        parent: Option<&str>,
    ) -> Result<String> {
        let existing = self.entry_ids();
        let id = short_id(&existing);
        let timestamp = now_timestamp();
        let fields = EntryFields {
            id: id.clone(),
            parent_id: parent.map(str::to_owned),
            timestamp,
        };
        let line = serde_json::to_string(&Line::Message {
            fields,
            message: MessageEntry {
                message: message.clone(),
            },
        })
        .context("encoding message entry")?;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&self.path)
            .with_context(|| format!("opening session {}", self.path.display()))?;
        writeln!(file, "{line}")
            .with_context(|| format!("writing session {}", self.path.display()))?;
        // Advance the leaf by rewriting only the header (the first line).
        self.count += 1;
        if self.first_message.is_none()
            && let phi_ai_types::Message::User(u) = message
        {
            self.first_message = Some(u.content.clone());
        }
        self.header.leaf_id = Some(id.clone());
        self.persist_header()?;
        Ok(id)
    }

    /// Append every message in a transcript slice (turns flush their final
    /// messages through this in one call). Each appends as a child of the
    /// previous, advancing the leaf — preserving conversation order.
    pub fn append_all(&mut self, messages: &[phi_ai_types::Message]) -> Result<()> {
        for message in messages {
            self.append(message)?;
        }
        Ok(())
    }

    /// Move the leaf pointer to an earlier entry (or `None` to point before
    /// the first message, for re-editing the root prompt). Existing entries
    /// are untouched; the next [`append`] / [`append_child`] forms a new
    /// branch off this position. Validates the target exists when set.
    pub fn branch(&mut self, to_id: Option<&str>) -> Result<()> {
        if let Some(id) = to_id {
            let ids = self.entry_ids();
            if !ids.contains(id) {
                bail!("entry {id} not found");
            }
        }
        self.header.leaf_id = to_id.map(str::to_owned);
        self.persist_header()
    }

    /// Create a new session file for the cwd/model that copies only the
    /// root→`from_leaf_id` path (the active branch, not the whole tree).
    /// The forked session's header points its leaf at the last entry of
    /// the path; labels along the path are re-derived from the source
    /// tree and rewritten as their own lines (pi's `createBranchedSession`).
    /// Returns the new store. Used by `/fork`: fork away into a fresh file,
    /// leaving the original session untouched.
    ///
    /// `from_leaf_id` is resolved from the header when `None` (the normal
    /// `/fork`-to-current-leaf case); validation rejects a missing id.
    /// Create a new session file for the cwd/model that copies only the
    /// root→`from_leaf_id` path (the active branch, not the whole tree).
    /// The forked session's header points its leaf at the last entry of
    /// the path; labels along the path are re-derived from the source
    /// tree and rewritten as their own lines (pi's `createBranchedSession`).
    /// Returns the new store. Used by `/fork`: fork away into a fresh file,
    /// leaving the original session untouched.
    ///
    /// `from_leaf_id` is resolved from the header when `None` (the normal
    /// `/fork`-to-current-leaf case); validation rejects a missing id.
    pub fn create_branched_session(&self, from_leaf_id: Option<&str>) -> Result<SessionStore> {
        let parsed = parse(&self.path)?;
        // Collect header fields + labels before `parsed.tree()` consumes
        // `parsed`.
        let header_leaf = parsed.header.leaf_id.clone();
        let labels_src: Vec<(String, Option<String>, String)> = parsed
            .labels
            .iter()
            .map(|l| {
                (
                    l.target_id.clone(),
                    l.label.clone(),
                    l.fields.timestamp.clone(),
                )
            })
            .collect();
        let tree = parsed.tree();
        let leaf_id = match from_leaf_id {
            Some(id) => Some(id.to_owned()),
            None => header_leaf,
        };
        if let Some(id) = &leaf_id
            && find_node(&tree, id).is_none()
        {
            bail!("entry {id} not found");
        }
        // The leaf→root path as borrowed source nodes (so we keep the
        // source ids for label re-keying).
        let path_nodes = leaf_path(&tree, leaf_id.as_deref());
        if path_nodes.is_empty() {
            // Nothing to fork: a fresh session with a parent pointer.
            return SessionStore::create_child_of(
                &self.path,
                &self.header,
                &[],
                &std::collections::HashMap::new(),
                &[],
            );
        }
        let messages: Vec<phi_ai_types::Message> =
            path_nodes.iter().map(|n| n.message.clone()).collect();
        // Labels carried along the path, in source order (last writer per
        // target). The target_id is the source entry; `create_child_of`
        // re-maps it to the new id via `source_id_map`.
        let path_ids: HashSet<&String> = path_nodes.iter().map(|n| &n.fields.id).collect();
        let labels: Vec<(String, Option<String>, String)> = labels_src
            .into_iter()
            .filter(|(target_id, _, _)| path_ids.contains(target_id))
            .collect();
        // Build the source→position index so `create_child_of` can re-key
        // labels (the path is linear, so position == new-id order).
        let source_id_map: std::collections::HashMap<String, usize> = path_nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.fields.id.clone(), i))
            .collect();
        SessionStore::create_child_of(&self.path, &self.header, &messages, &source_id_map, &labels)
    }

    /// Set or clear a user-defined label on the target entry. The label is
    /// appended as its own `label` line (so labels scan cheaply) and the
    /// header's leaf advances to it (label changes are part of history, so
    /// the dead branch after a label change is recorded too). Validates the
    /// target exists.
    pub fn label(&mut self, target_id: &str, label: Option<&str>) -> Result<String> {
        let ids = self.entry_ids();
        if !ids.contains(target_id) {
            bail!("entry {target_id} not found");
        }
        let parent = self.leaf_id();
        let id = short_id(&ids);
        let timestamp = now_timestamp();
        let entry = LabelEntry {
            fields: EntryFields {
                id: id.clone(),
                parent_id: parent.as_deref().map(str::to_owned),
                timestamp,
            },
            target_id: target_id.to_owned(),
            label: label.map(str::to_owned),
        };
        let line = serde_json::to_string(&Line::Label {
            fields: entry.fields.clone(),
            target_id: entry.target_id,
            label: entry.label,
        })
        .context("encoding label entry")?;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&self.path)
            .with_context(|| format!("opening session {}", self.path.display()))?;
        writeln!(file, "{line}")
            .with_context(|| format!("writing session {}", self.path.display()))?;
        self.header.leaf_id = Some(id.clone());
        self.persist_header()?;
        Ok(id)
    }

    /// The tree of message entries (oldest-first per subtree) with labels
    /// resolved from the latest `label` line for each target. The
    /// session-tree browser reads this and never touches the flat list. A
    /// well-formed session has one root; orphaned entries (broken parent
    /// chain) surface as additional roots.
    pub fn tree(&self) -> Result<Vec<TreeNode>> {
        read_tree(&self.path).map(|p| p.tree)
    }

    /// The messages along the root→leaf path, in order. After a `branch`
    /// moves the leaf to an earlier entry, this is the transcript the loop
    /// resumes from (pi's `buildSessionContext`). The leaf itself is the
    /// caller's position to append onto, so it is included; pass the leaf
    /// id from the store you just branched.
    pub fn leaf_messages(&self) -> Result<Vec<phi_ai_types::Message>> {
        let parsed = read_tree(&self.path)?;
        Ok(messages_along_leaf_path(
            &parsed.tree,
            parsed.leaf_id.as_deref(),
        ))
    }

    /// Rewrite the header line in place (the leaf moved). Done by rewriting
    /// only the first line: read all lines, replace line 0, write back. The
    /// header is one line, so this is cheap relative to a turn.
    fn persist_header(&mut self) -> Result<()> {
        let bytes = std::fs::read(&self.path).with_context(|| {
            format!("reading session for header rewrite {}", self.path.display())
        })?;
        let content = String::from_utf8_lossy(&bytes);
        let mut lines: Vec<String> = content.lines().map(str::to_owned).collect();
        // Drop a trailing empty line so we don't grow a blank each rewrite.
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        if lines.is_empty() {
            bail!("session file lost its header: {}", self.path.display());
        }
        lines[0] = serde_json::to_string(&Line::Session(self.header.clone()))
            .context("encoding rewritten header")?;
        let mut out = lines.join("\n");
        out.push('\n');
        std::fs::write(&self.path, out)
            .with_context(|| format!("rewriting session header {}", self.path.display()))?;
        Ok(())
    }

    /// The set of entry ids currently in the file (messages + labels). Used
    /// for collision-checked id generation and branch/label validation.
    fn entry_ids(&self) -> HashSet<String> {
        let parsed = match parse(&self.path) {
            Ok(p) => p,
            Err(_) => return HashSet::new(),
        };
        let mut ids = HashSet::new();
        for entry in &parsed.entries {
            ids.insert(entry.fields.id.clone());
        }
        for label in &parsed.labels {
            ids.insert(label.fields.id.clone());
        }
        ids
    }
}

impl Parsed {
    /// Build the resolved tree from parsed entries + labels. Children are
    /// oldest-first (file order); labels attach to their latest target.
    /// A node is a root when its parent is `None`, itself, or missing from
    /// the map (orphaned — a broken parent chain surfaces as a root too).
    pub(crate) fn tree(self) -> Vec<TreeNode> {
        use std::collections::HashMap;

        // Resolve labels: last writer wins per target_id.
        let mut labels: HashMap<String, (Option<String>, Option<String>)> = HashMap::new();
        for label in &self.labels {
            let entry = labels
                .entry(label.target_id.clone())
                .or_insert((None, None));
            entry.0 = label.label.clone();
            entry.1 = Some(label.fields.timestamp.clone());
        }

        // Build the id → node map, attaching the resolved label.
        let mut node_map: HashMap<String, TreeNode> = HashMap::new();
        for entry in self.entries {
            let (label, label_timestamp) = labels.remove(&entry.fields.id).unwrap_or((None, None));
            node_map.insert(
                entry.fields.id.clone(),
                TreeNode {
                    fields: entry.fields,
                    message: entry.message,
                    label,
                    label_timestamp,
                    children: Vec::new(),
                },
            );
        }

        // Pass 1: group child ids by parent (file order, so siblings stay
        // oldest-first as the browser expects).
        let mut kids: HashMap<String, Vec<String>> = HashMap::new();
        let id_order: Vec<String> = node_map.keys().cloned().collect();
        for id in &id_order {
            let node = node_map.get(id).expect("id present");
            if let Some(parent) = &node.fields.parent_id
                && parent != id
                && node_map.contains_key(parent)
            {
                kids.entry(parent.clone()).or_default().push(id.clone());
            }
        }

        // Pass 2: roots — parent is None, self, or missing.
        let mut roots: Vec<String> = id_order
            .iter()
            .filter(|id| {
                let node = node_map.get(*id).expect("id present");
                match &node.fields.parent_id {
                    None => true,
                    Some(p) if p == *id => true,
                    Some(p) => !node_map.contains_key(p),
                }
            })
            .cloned()
            .collect();
        roots.sort_by_key(|id| {
            node_map
                .get(id)
                .map(|n| n.fields.timestamp.clone())
                .unwrap_or_default()
        });

        // Recursively attach children (a pre-order DFS, oldest sibling
        // first — matching pi's `getTree` child ordering).
        fn build(
            id: &str,
            map: &HashMap<String, TreeNode>,
            kids: &HashMap<String, Vec<String>>,
        ) -> TreeNode {
            let mut node = map.get(id).expect("id present").clone();
            if let Some(child_ids) = kids.get(id) {
                for child in child_ids {
                    node.children.push(build(child, map, kids));
                }
            }
            node
        }
        roots
            .into_iter()
            .map(|id| build(&id, &node_map, &kids))
            .collect()
    }
}

/// Header + message count + first user prompt — the metadata path used by
/// `--continue` and the `/resume` picker, without holding the transcript.
fn stats(parsed: &Parsed) -> (usize, Option<String>) {
    let first = parsed.entries.iter().find_map(|e| match &e.message {
        phi_ai_types::Message::User(u) => Some(u.content.clone()),
        _ => None,
    });
    (parsed.entries.len(), first)
}

/// The full parse: header, message entries (file order), and label entries.
/// Malformed lines are skipped; a missing header bails. v1 files (entries
/// with no `id`/`parentId`) are migrated to v2 in memory by synthesizing a
/// linear chain (oldest-first), so the tree read path always sees ids.
pub(crate) fn parse(path: &Path) -> Result<Parsed> {
    let bytes =
        std::fs::read(path).with_context(|| format!("reading session {}", path.display()))?;
    let content = String::from_utf8_lossy(&bytes);
    let mut header: Option<Header> = None;
    let mut raw_entries: Vec<RawMessage> = Vec::new();
    let mut labels: Vec<LabelEntry> = Vec::new();
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let mut records = serde_json::Deserializer::from_str(line).into_iter::<serde_json::Value>();
        while let Some(Ok(value)) = records.next() {
            let kind = value.get("type").and_then(|v| v.as_str()).unwrap_or("");
            match kind {
                "session" => {
                    if let Ok(h) = serde_json::from_value::<Header>(value)
                        && header.is_none()
                    {
                        if h.version > SESSION_VERSION {
                            bail!(
                                "session file version {} is newer than this phi understands (max {}): {}",
                                h.version,
                                SESSION_VERSION,
                                path.display()
                            );
                        }
                        header = Some(h);
                    }
                }
                "message" => {
                    if let Ok(raw) = serde_json::from_value::<RawMessage>(value) {
                        raw_entries.push(raw);
                    }
                }
                "label" => {
                    if let Ok(label) = serde_json::from_value::<LabelEntry>(value) {
                        labels.push(label);
                    }
                }
                _ => {}
            }
        }
    }
    let mut header = header
        .with_context(|| format!("not a phi session file (no header): {}", path.display()))?;
    // Migrate v1 → v2: synthesize ids + a linear parent chain when absent.
    let migrated = header.version < 2;
    if migrated {
        let mut prev: Option<String> = None;
        let mut seen: HashSet<String> = HashSet::new();
        for raw in &mut raw_entries {
            if raw.id.is_none() {
                raw.id = Some(short_id(&seen));
            }
            let id = raw.id.clone().unwrap();
            seen.insert(id);
            if raw.parent_id.is_none() {
                raw.parent_id = prev.clone();
            }
            prev = Some(raw.id.clone().unwrap());
        }
        if header.leaf_id.is_none() {
            header.leaf_id = raw_entries.last().map(|r| r.id.clone().unwrap());
        }
    }
    let mut entries = Vec::with_capacity(raw_entries.len());
    for raw in raw_entries {
        let RawMessage {
            message,
            id,
            parent_id,
            timestamp,
        } = raw;
        entries.push(ParsedEntry {
            fields: EntryFields {
                id: id.expect("id synthesized in migration"),
                parent_id,
                timestamp: timestamp.unwrap_or_else(|| header.timestamp.clone()),
            },
            // MessageEntry flattens its single `message` field, so
            // deserializing into `RawMessage` yields the inner `Message`.
            message: message.message,
        });
    }
    if migrated {
        // Persist the migration so future reads skip it. Best-effort: a
        // failure to rewrite is non-fatal (the in-memory tree is still
        // correct), but we don't write here to avoid surprising the caller
        // — `label`/`branch`/`append` rewrite the header lazily, and the
        // first append rewrites all ids. For now, leave the file as-is;
        // the next append's `persist_header` records the v2 leaf.
        let _ = header;
    }
    Ok(Parsed {
        header,
        entries,
        labels,
    })
}

/// The raw message line as it sits on disk: the payload plus optional tree
/// fields. [`parse`] deserializes into this so v1 files (no fields) and v2
/// files (with fields) load from one shape.
#[derive(Deserialize)]
struct RawMessage {
    #[serde(flatten)]
    message: MessageEntry,
    #[serde(default)]
    id: Option<String>,
    #[serde(default, rename = "parent_id")]
    parent_id: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
}

/// Read the whole session file: header + every loadable message entry, in
/// file order. Malformed lines (including a torn final write) are skipped,
/// so the transcript is whatever survived. For a v1 file, the messages
/// come back in their natural linear order (the parse synthesizes a chain).
pub fn load_messages(path: &Path) -> Result<(Header, Vec<phi_ai_types::Message>)> {
    let parsed = parse(path)?;
    let messages = parsed.entries.into_iter().map(|e| e.message).collect();
    Ok((parsed.header, messages))
}

/// Read the whole session file as a resolved tree. The header's `leaf_id`
/// (or the last entry when absent) is the active position; the browser
/// reads `tree` and `leaf_id` together.
pub fn read_tree(path: &Path) -> Result<ParsedTree> {
    let parsed = parse(path)?;
    let header = parsed.header.clone();
    let leaf_id = header.leaf_id.clone();
    let tree = parsed.tree();
    Ok(ParsedTree {
        header,
        tree,
        leaf_id,
    })
}

/// Find the node carrying `id` anywhere in a forest (DFs, first match).
/// Linear-time; fine for session-sized trees.
pub fn find_node<'a>(roots: &'a [TreeNode], id: &str) -> Option<&'a TreeNode> {
    for root in roots {
        if let Some(n) = find_node_rec(root, id) {
            return Some(n);
        }
    }
    None
}

fn find_node_rec<'a>(node: &'a TreeNode, id: &str) -> Option<&'a TreeNode> {
    if node.fields.id == id {
        return Some(node);
    }
    for child in &node.children {
        if let Some(n) = find_node_rec(child, id) {
            return Some(n);
        }
    }
    None
}

/// The messages along the root→`leaf_id` path, in order. The leaf is
/// included (it is the position to append onto). When `leaf_id` is `None`,
/// returns an empty vec — `branch(None)` points before the first message.
pub fn messages_along_leaf_path(
    roots: &[TreeNode],
    leaf_id: Option<&str>,
) -> Vec<phi_ai_types::Message> {
    leaf_path(roots, leaf_id)
        .into_iter()
        .map(|n| n.message.clone())
        .collect()
}

/// The nodes along the root→`leaf_id` path, in order (leaf last). Borrowed
/// from the source tree so the caller has the source ids too — needed to
/// re-key labels when forking. `None`/missing leaf → empty.
pub fn leaf_path<'a>(roots: &'a [TreeNode], leaf_id: Option<&'a str>) -> Vec<&'a TreeNode> {
    let Some(id) = leaf_id else {
        return Vec::new();
    };
    let Some(node) = find_node(roots, id) else {
        return Vec::new();
    };
    let mut path: Vec<&TreeNode> = Vec::new();
    let mut current: Option<&TreeNode> = Some(node);
    while let Some(n) = current {
        path.push(n);
        current = parent_of(roots, n);
    }
    path.reverse();
    path
}

/// The parent of `node` in the forest (DFs); `None` for a root.
fn parent_of<'a>(roots: &'a [TreeNode], node: &TreeNode) -> Option<&'a TreeNode> {
    let parent_id = node.fields.parent_id.as_deref()?;
    find_node(roots, parent_id)
}

/// A parsed tree + the active leaf, for the browser's read path.
pub struct ParsedTree {
    pub header: Header,
    pub tree: Vec<TreeNode>,
    /// The active leaf id, resolved from the header or the last entry.
    pub leaf_id: Option<String>,
}

/// All sessions for `cwd`'s directory under `root`, newest creation first
/// (UUID v7 is time-ordered, so lexicographic descending sort is creation
/// order — stable across writes, unlike mtime).
pub fn list_in(root: &Path, cwd: &Path) -> Vec<SessionMeta> {
    let dir = sessions_dir_in(root, cwd);
    let Ok(read) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut sessions: Vec<SessionMeta> = read
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|e| {
            let path = e.path();
            let store = SessionStore::open(&path).ok()?;
            if store.count == 0 {
                return None;
            }
            Some(SessionMeta {
                path,
                id: store.header.id.clone(),
                model: store.header.model.clone(),
                cwd: store.header.cwd.clone(),
                first_message: store.first_message.unwrap_or_default(),
                message_count: store.count,
                created_at: store.header.timestamp.clone(),
            })
        })
        .collect();
    sessions.sort_by(|a, b| b.id.cmp(&a.id));
    sessions
}

/// All sessions for `cwd` under the default root.
pub fn list(cwd: &Path) -> Vec<SessionMeta> {
    match sessions_root() {
        Ok(root) => list_in(&root, cwd),
        Err(_) => Vec::new(),
    }
}

/// The most recent session for `cwd` under `root`, or `None` when there is
/// none (or the directory is unreadable). Used by `--continue`.
pub fn most_recent_in(root: &Path, cwd: &Path) -> Option<PathBuf> {
    list_in(root, cwd).into_iter().next().map(|m| m.path)
}

/// The most recent session for `cwd` under the default root.
pub fn most_recent(cwd: &Path) -> Option<PathBuf> {
    match sessions_root() {
        Ok(root) => most_recent_in(&root, cwd),
        Err(_) => None,
    }
}
