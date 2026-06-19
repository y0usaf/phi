use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;

use anyhow::Result;
use rusqlite::{Connection, params};

/// A single memory node in the cognitive graph.
#[derive(Debug, Clone)]
pub struct Memory {
    pub id: i64,
    pub title: String,
    pub body: String,
    pub kind: String,
    pub created_at: i64,
    pub accessed_at: i64,
    pub access_count: i64,
    pub strength: f64,   // learned Q-value [0,1]
    pub warmth: f64,     // salience [0,1]
    pub activation: f64, // cached ACT-R base-level activation
}

/// A directed edge in the knowledge graph.
#[derive(Debug, Clone)]
pub struct Edge {
    pub id: i64,
    pub source: i64,
    pub target: i64,
    pub relation: String,
    pub weight: f64,
    pub created_at: i64,
}

/// Fusion weights for the retrieval pipeline. Ori-mnemos runs semantic+BM25+
/// PageRank+warmth via reciprocal-rank fusion; we expose the same knobs.
#[derive(Debug, Clone)]
pub struct FusionWeights {
    pub bm25: f64,
    pub pagerank: f64,
    pub activation: f64,
    pub warmth: f64,
    pub strength: f64,
    pub recency: f64,
}

impl Default for FusionWeights {
    fn default() -> Self {
        Self {
            bm25: 0.40,
            pagerank: 0.20,
            activation: 0.15,
            warmth: 0.05,
            strength: 0.10,
            recency: 0.10,
        }
    }
}

/// ACT-R decay parameter (d = 0.5 in the standard memory model).
const DECAY_D: f64 = 0.5;
/// PageRank damping factor.
const PR_DAMPING: f64 = 0.85;
/// PageRank iteration count.
const PR_ITERS: usize = 12;
/// Activation floor below which a memory is eligible for pruning.
const PRUNE_THRESHOLD: f64 = -8.0;

pub struct MemoryStore {
    conn: Mutex<Connection>,
    fusion: Mutex<FusionWeights>,
}

/// Graph neighborhood: each node id maps to its weighted outgoing edges
/// `(neighbor id, edge weight)`, plus the full set of visited node ids.
type GraphNeighborhood = (HashMap<i64, Vec<(i64, f64)>>, HashSet<i64>);

impl MemoryStore {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        let store = Self {
            conn: Mutex::new(conn),
            fusion: Mutex::new(FusionWeights::default()),
        };
        store.init()?;
        Ok(store)
    }

    pub fn set_fusion_weights(&self, w: FusionWeights) {
        *self.fusion.lock().unwrap() = w;
    }

    fn init(&self) -> Result<()> {
        self.conn.lock().unwrap().execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;

            CREATE TABLE IF NOT EXISTS memories (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                title        TEXT    NOT NULL,
                body         TEXT    NOT NULL,
                kind         TEXT    NOT NULL DEFAULT 'note',
                created_at   INTEGER NOT NULL,
                accessed_at  INTEGER NOT NULL,
                access_count INTEGER NOT NULL DEFAULT 0,
                strength     REAL    NOT NULL DEFAULT 0.0,
                warmth       REAL    NOT NULL DEFAULT 0.5,
                activation   REAL    NOT NULL DEFAULT 0.0,
                pruned       INTEGER NOT NULL DEFAULT 0
            );

            CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
                title, body,
                content='memories',
                content_rowid='id'
            );

            CREATE TRIGGER IF NOT EXISTS mem_ai AFTER INSERT ON memories BEGIN
                INSERT INTO memories_fts(rowid, title, body) VALUES (new.id, new.title, new.body);
            END;
            CREATE TRIGGER IF NOT EXISTS mem_ad AFTER DELETE ON memories BEGIN
                INSERT INTO memories_fts(memories_fts, rowid, title, body) VALUES('delete', old.id, old.title, old.body);
            END;
            CREATE TRIGGER IF NOT EXISTS mem_au AFTER UPDATE ON memories BEGIN
                INSERT INTO memories_fts(memories_fts, rowid, title, body) VALUES('delete', old.id, old.title, old.body);
                INSERT INTO memories_fts(rowid, title, body) VALUES (new.id, new.title, new.body);
            END;

            CREATE TABLE IF NOT EXISTS edges (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                source     INTEGER NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
                target     INTEGER NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
                relation   TEXT    NOT NULL DEFAULT 'related',
                weight     REAL    NOT NULL DEFAULT 0.5,
                created_at INTEGER NOT NULL,
                UNIQUE(source, target, relation)
            );
            CREATE INDEX IF NOT EXISTS idx_edges_source ON edges(source);
            CREATE INDEX IF NOT EXISTS idx_edges_target ON edges(target);

            CREATE TABLE IF NOT EXISTS practice (
                id        INTEGER PRIMARY KEY AUTOINCREMENT,
                memory_id INTEGER NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
                ts        INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_practice_mem ON practice(memory_id);
            "#,
        )?;
        Ok(())
    }

    // -- Capture -------------------------------------------------------------

    /// Insert a memory, auto-extract `[[wiki-links]]` from the body into edges,
    /// and record an initial practice event. Returns the new memory id.
    pub fn capture(&self, title: &str, body: &str, kind: &str) -> Result<i64> {
        let mut conn = self.conn.lock().unwrap();
        let now = unix_now();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO memories (title, body, kind, created_at, accessed_at, strength, warmth, activation)
             VALUES (?1, ?2, ?3, ?4, ?4, 0.0, 0.5, 0.0)",
            params![title, body, kind, now],
        )?;
        let id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO practice (memory_id, ts) VALUES (?1, ?2)",
            params![id, now],
        )?;
        // Auto-link wiki-links: [[Target Title]] -> edge to a memory with that title.
        // Forward links: [[Target]] in this body -> existing memories with that title.
        let links = extract_wiki_links(body);
        for link_target in links {
            if let Ok(target_id) = tx
                .query_row(
                    "SELECT id FROM memories WHERE title = ?1 AND pruned = 0",
                    params![link_target],
                    |r| r.get::<_, i64>(0),
                )
                && target_id != id {
                    tx.execute(
                        "INSERT OR IGNORE INTO edges (source, target, relation, weight, created_at)
                         VALUES (?1, ?2, 'derived', 0.5, ?3)",
                        params![id, target_id, now],
                    )?;
                }
        }
        // Back links: existing memories whose bodies contain [[this title]].
        let pattern = format!("%[[{}]]%", escape_like_pattern(title));
        let mut back_stmt =
            tx.prepare("SELECT id FROM memories WHERE id != ?1 AND pruned = 0 AND body LIKE ?2")?;
        let back_ids: Vec<i64> = back_stmt
            .query_map(params![id, pattern], |r| r.get::<_, i64>(0))?
            .filter_map(|r| r.ok())
            .collect();
        drop(back_stmt);
        for source_id in back_ids {
            tx.execute(
                "INSERT OR IGNORE INTO edges (source, target, relation, weight, created_at)
                 VALUES (?1, ?2, 'derived', 0.5, ?3)",
                params![source_id, id, now],
            )?;
        }
        tx.commit()?;
        drop(conn);
        self.recompute_activation(id)?;
        Ok(id)
    }

    /// Create an explicit typed edge between two memories.
    pub fn link(&self, source: i64, target: i64, relation: &str, weight: f64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = unix_now();
        conn.execute(
            "INSERT OR IGNORE INTO edges (source, target, relation, weight, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![source, target, relation, weight, now],
        )?;
        Ok(())
    }

    // -- Recall (fusion retrieval) -------------------------------------------

    /// Hybrid retrieval: FTS5 BM25 candidate generation (OR-joined so
    /// multi-word queries match) followed by fusion reranking across
    /// BM25 . PageRank . activation . warmth . strength . recency.
    pub fn recall(&self, query: &str, limit: usize) -> Result<Vec<Memory>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let candidate_ids = self.bm25_candidates(query, (limit * 4).max(20))?;
        if candidate_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut ranked = self.fusion_rank(&candidate_ids, query)?;
        let memories: Vec<Memory> = ranked.drain(..limit.min(ranked.len())).collect();
        self.bump_access(&memories.iter().map(|m| m.id).collect::<Vec<_>>())?;
        // Strengthen co-occurrence edges among simultaneously-recalled memories
        // (Hebbian learning: neurons that fire together wire together).
        self.strengthen_cooccurrence(&memories)?;
        Ok(memories)
    }

    /// Async wrapper for [`recall`](Self::recall) that offloads synchronous
    /// SQLite work to a blocking thread so the async executor is not stalled.
    pub async fn recall_async(
        self: std::sync::Arc<Self>,
        query: String,
        limit: usize,
    ) -> Result<Vec<Memory>> {
        let store = self;
        tokio::task::spawn_blocking(move || store.recall(&query, limit))
            .await?
    }

    /// Raw FTS5 BM25 candidate fetch. Tokens are OR-joined so a natural-language
    /// prompt like "tell me about sqlite search" still matches relevant rows.
    fn bm25_candidates(&self, query: &str, limit: usize) -> Result<Vec<i64>> {
        let conn = self.conn.lock().unwrap();
        let fts_query = escape_fts_or(query);
        if fts_query.is_empty() {
            return Ok(Vec::new());
        }
        let mut stmt = conn.prepare(
            "SELECT m.id
             FROM memories m
             JOIN memories_fts fts ON m.id = fts.rowid
             WHERE memories_fts MATCH ?1 AND m.pruned = 0
             ORDER BY bm25(memories_fts)
             LIMIT ?2",
        )?;
        let ids: Vec<i64> = stmt
            .query_map(params![fts_query, limit as i64], |r| r.get::<_, i64>(0))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(ids)
    }

    /// Fuse multiple signals into a single ranking. Mirrors ori-mnemos L2/L4:
    /// BM25 rank + graph centrality (PageRank) + ACT-R decay + learned
    /// Q-value (strength) + warmth + recency.
    fn fusion_rank(&self, ids: &[i64], _query: &str) -> Result<Vec<Memory>> {
        let now = unix_now();
        let rows: Vec<Memory> = {
            let conn = self.conn.lock().unwrap();
            let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!(
                "SELECT id, title, body, kind, created_at, accessed_at, access_count,
                        strength, warmth, activation
                 FROM memories WHERE id IN ({placeholders}) AND pruned = 0"
            );
            let mut stmt = conn.prepare(&sql)?;
            let bindings = rusqlite::params_from_iter(ids.iter());
            stmt.query_map(bindings, |r| row_to_memory(r, now))?
                .filter_map(|r| r.ok())
                .collect()
        };

        let bm25_rank: HashMap<i64, usize> =
            ids.iter().enumerate().map(|(i, &id)| (id, i)).collect();
        let pr = self.pagerank_map()?;
        let w = self.fusion.lock().unwrap().clone();

        let mut scored: Vec<(f64, Memory)> = rows
            .into_iter()
            .map(|m| {
                let bm25_n = rank_norm(
                    bm25_rank.get(&m.id).copied().unwrap_or(usize::MAX),
                    ids.len(),
                );
                let pr_n = pr.get(&m.id).copied().unwrap_or(0.0);
                let act_n = sigmoid(m.activation);
                let str_n = m.strength.clamp(0.0, 1.0);
                let warm_n = m.warmth.clamp(0.0, 1.0);
                let rec_n = recency_norm(m.accessed_at, now);
                let score = w.bm25 * bm25_n
                    + w.pagerank * pr_n
                    + w.activation * act_n
                    + w.strength * str_n
                    + w.warmth * warm_n
                    + w.recency * rec_n;
                (score, m)
            })
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        Ok(scored.into_iter().map(|(_, m)| m).collect())
    }

    // -- Recursive exploration (PPR walk) ------------------------------------

    /// Personalized PageRank walk from BM25 seeds, returning discovered
    /// memories (excluding the seeds) ranked by propagation score. This is
    /// ori-mnemos L5: recursive exploration of the graph neighborhood.
    pub fn explore(&self, query: &str, limit: usize) -> Result<Vec<Memory>> {
        let seeds = self.bm25_candidates(query, 5)?;
        if seeds.is_empty() {
            return Ok(Vec::new());
        }
        let now = unix_now();
        let (adjacency, node_set) = self.graph_around(&seeds, 4)?;
        if node_set.is_empty() {
            return Ok(Vec::new());
        }
        // PPR: teleport only from seeds, follow weighted edges.
        let mut score: HashMap<i64, f64> = HashMap::new();
        let seed_weight = 1.0 / seeds.len() as f64;
        for &s in &seeds {
            *score.entry(s).or_insert(0.0) += seed_weight;
        }
        for _ in 0..20 {
            let mut next: HashMap<i64, f64> = HashMap::new();
            for (&node, &s) in &score {
                let teleport = if seeds.contains(&node) {
                    PR_DAMPING * s
                } else {
                    0.0
                };
                *next.entry(node).or_insert(0.0) += teleport;
                let spread = (1.0 - PR_DAMPING) * s;
                if let Some(neighbors) = adjacency.get(&node) {
                    let total_w: f64 = neighbors.iter().map(|(_, w)| *w).sum();
                    if total_w > 0.0 {
                        for &(tgt, w) in neighbors {
                            *next.entry(tgt).or_insert(0.0) += spread * (w / total_w);
                        }
                    }
                }
            }
            score = next;
        }
        // Exclude seeds; return top-N discovered.
        let mut discovered: Vec<(f64, i64)> = score
            .into_iter()
            .filter(|(id, _)| !seeds.contains(id))
            .filter(|(id, _)| node_set.contains(id))
            .map(|(id, s)| (s, id))
            .collect();
        discovered.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let top_ids: Vec<i64> = discovered
            .into_iter()
            .take(limit)
            .map(|(_, id)| id)
            .collect();
        if top_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = top_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT id, title, body, kind, created_at, accessed_at, access_count,
                    strength, warmth, activation
             FROM memories WHERE id IN ({placeholders}) AND pruned = 0"
        );
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&sql)?;
        let bindings = rusqlite::params_from_iter(top_ids.iter());
        let mut memories: Vec<Memory> = stmt
            .query_map(bindings, |r| row_to_memory(r, now))?
            .filter_map(|r| r.ok())
            .collect();
        // preserve PPR order
        memories.sort_by_key(|m| {
            top_ids
                .iter()
                .position(|&id| id == m.id)
                .unwrap_or(usize::MAX)
        });
        Ok(memories)
    }

    // -- Learning ------------------------------------------------------------

    /// Reinforce a memory (positive reward raises its Q-value / strength and
    /// records a practice event). Used after a recalled memory proves useful.
    pub fn reflect(&self, id: i64, reward: f64) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let now = unix_now();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO practice (memory_id, ts) VALUES (?1, ?2)",
            params![id, now],
        )?;
        // Q-learning-ish update: strength <- strength + alpha(reward - strength)
        let alpha = 0.2;
        tx.execute(
            "UPDATE memories SET strength = MIN(1.0, strength + ?1 * (?2 - strength)),
                                 warmth = MIN(1.0, warmth + 0.05 * ?2),
                                 accessed_at = ?3
             WHERE id = ?4",
            params![alpha, reward, now, id],
        )?;
        tx.commit()?;
        drop(conn);
        self.recompute_activation(id)?;
        Ok(())
    }

    /// Hebbian: bump edge weight between simultaneously-recalled memories.
    fn strengthen_cooccurrence(&self, memories: &[Memory]) -> Result<()> {
        if memories.len() < 2 {
            return Ok(());
        }
        let conn = self.conn.lock().unwrap();
        let now = unix_now();
        for i in 0..memories.len() {
            for j in (i + 1)..memories.len() {
                let (a, b) = (memories[i].id, memories[j].id);
                conn.execute(
                    "INSERT INTO edges (source, target, relation, weight, created_at)
                     VALUES (?1, ?2, 'cooccurs', 0.1, ?3)
                     ON CONFLICT(source, target, relation) DO UPDATE SET
                         weight = MIN(1.0, weight + 0.05)",
                    params![a, b, now],
                )?;
                conn.execute(
                    "INSERT INTO edges (source, target, relation, weight, created_at)
                     VALUES (?1, ?2, 'cooccurs', 0.1, ?3)
                     ON CONFLICT(source, target, relation) DO UPDATE SET
                         weight = MIN(1.0, weight + 0.05)",
                    params![b, a, now],
                )?;
            }
        }
        Ok(())
    }

    // -- Decay & pruning -----------------------------------------------------

    /// Recompute ACT-R base-level activation for one memory from its practice
    /// history: B_i = ln( sum ( (t_now - t_j)^(-d) ) ).
    fn recompute_activation(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = unix_now();
        let times: Vec<i64> = {
            let mut stmt =
                conn.prepare("SELECT ts FROM practice WHERE memory_id = ?1 ORDER BY ts")?;
            stmt.query_map(params![id], |r| r.get::<_, i64>(0))?
                .filter_map(|r| r.ok())
                .collect()
        };
        let activation = if times.is_empty() {
            0.0
        } else {
            let sum: f64 = times
                .iter()
                .filter(|&&t| t < now)
                .map(|&t| {
                    let dt = (now - t) as f64;
                    if dt > 0.0 { dt.powf(-DECAY_D) } else { 1.0 }
                })
                .sum();
            if sum > 0.0 { sum.ln() } else { 0.0 }
        };
        conn.execute(
            "UPDATE memories SET activation = ?1 WHERE id = ?2",
            params![activation, id],
        )?;
        Ok(())
    }

    /// Recompute activation for all non-pruned memories, then soft-prune any
    /// below the activation floor. Call periodically (e.g. on session start).
    pub fn decay_all(&self) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let ids: Vec<i64> = {
            let mut stmt = conn.prepare("SELECT id FROM memories WHERE pruned = 0")?;
            stmt.query_map([], |r| r.get::<_, i64>(0))?
                .filter_map(|r| r.ok())
                .collect()
        };
        drop(conn);
        for &id in &ids {
            self.recompute_activation(id)?;
        }
        let conn = self.conn.lock().unwrap();
        let pruned = conn.execute(
            "UPDATE memories SET pruned = 1 WHERE activation < ?1 AND pruned = 0",
            params![PRUNE_THRESHOLD],
        )?;
        Ok(pruned)
    }

    /// Hard-delete a memory (also removes its FTS row and edges via cascade).
    pub fn forget(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM memories WHERE id = ?1", params![id])?;
        Ok(())
    }

    // -- Graph ---------------------------------------------------------------

    pub fn edges_of(&self, memory_id: i64) -> Result<Vec<Edge>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, source, target, relation, weight, created_at FROM edges
             WHERE source = ?1 OR target = ?1 ORDER BY weight DESC",
        )?;
        let edges = stmt
            .query_map(params![memory_id], |r| {
                Ok(Edge {
                    id: r.get(0)?,
                    source: r.get(1)?,
                    target: r.get(2)?,
                    relation: r.get(3)?,
                    weight: r.get(4)?,
                    created_at: r.get(5)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(edges)
    }

    /// Weighted PageRank over the full live graph.
    fn pagerank_map(&self) -> Result<HashMap<i64, f64>> {
        let conn = self.conn.lock().unwrap();
        let mut adj: HashMap<i64, Vec<(i64, f64)>> = HashMap::new();
        let mut nodes: HashSet<i64> = HashSet::new();
        let mut stmt = conn.prepare("SELECT source, target, weight FROM edges")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, f64>(2)?,
            ))
        })?;
        for row in rows {
            let (s, t, w) = row?;
            adj.entry(s).or_default().push((t, w));
            nodes.insert(s);
            nodes.insert(t);
        }
        if nodes.is_empty() {
            return Ok(HashMap::new());
        }
        let n = nodes.len() as f64;
        let mut pr: HashMap<i64, f64> = nodes.iter().map(|&id| (id, 1.0 / n)).collect();
        for _ in 0..PR_ITERS {
            let mut next: HashMap<i64, f64> = nodes
                .iter()
                .map(|&id| (id, (1.0 - PR_DAMPING) / n))
                .collect();
            for (&node, &score) in &pr {
                let neighbors = adj.get(&node);
                let out_w: f64 = neighbors
                    .map(|v| v.iter().map(|(_, w)| *w).sum())
                    .unwrap_or(0.0);
                if out_w > 0.0 {
                    for &(tgt, w) in neighbors.unwrap() {
                        *next.get_mut(&tgt).unwrap() += PR_DAMPING * score * (w / out_w);
                    }
                } else {
                    // dangle: redistribute uniformly
                    for &id in &nodes {
                        *next.get_mut(&id).unwrap() += PR_DAMPING * score / n;
                    }
                }
            }
            pr = next;
        }
        // normalize to [0,1]
        let max = pr.values().cloned().fold(0.0_f64, f64::max).max(1e-9);
        for v in pr.values_mut() {
            *v /= max;
        }
        Ok(pr)
    }

    /// Async wrapper for [`pagerank_map`](Self::pagerank_map) that offloads
    /// synchronous SQLite work to a blocking thread.
    #[allow(dead_code)]
    async fn pagerank_map_async(self: std::sync::Arc<Self>) -> Result<HashMap<i64, f64>> {
        let store = self;
        tokio::task::spawn_blocking(move || store.pagerank_map())
            .await?
    }

    /// Collect the graph neighborhood up to `depth` hops from `seeds`.
    fn graph_around(
        &self,
        seeds: &[i64],
        depth: usize,
    ) -> Result<GraphNeighborhood> {
        let conn = self.conn.lock().unwrap();
        let mut adj: HashMap<i64, Vec<(i64, f64)>> = HashMap::new();
        let mut frontier: HashSet<i64> = seeds.iter().copied().collect();
        let mut visited: HashSet<i64> = HashSet::new();
        for _ in 0..=depth {
            if frontier.is_empty() {
                break;
            }
            let layer: Vec<i64> = frontier.drain().collect();
            for id in &layer {
                visited.insert(*id);
            }
            if layer.is_empty() {
                break;
            }
            let placeholders = layer.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!(
                "SELECT source, target, weight FROM edges
                 WHERE source IN ({placeholders}) OR target IN ({placeholders})"
            );
            let mut stmt = conn.prepare(&sql)?;
            let bindings = rusqlite::params_from_iter(layer.iter().chain(layer.iter()));
            let rows = stmt.query_map(bindings, |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, f64>(2)?,
                ))
            })?;
            for row in rows {
                let (s, t, w) = row?;
                adj.entry(s).or_default().push((t, w));
                if !visited.contains(&s) {
                    frontier.insert(s);
                }
                if !visited.contains(&t) {
                    frontier.insert(t);
                }
                visited.insert(s);
                visited.insert(t);
            }
        }
        Ok((adj, visited))
    }

    // -- List / status -------------------------------------------------------

    pub fn list(&self, kind: Option<&str>, limit: usize) -> Result<Vec<Memory>> {
        let conn = self.conn.lock().unwrap();
        let now = unix_now();
        let memories: Vec<Memory> = match kind {
            Some(k) => {
                let mut stmt = conn.prepare(
                    "SELECT id, title, body, kind, created_at, accessed_at, access_count,
                            strength, warmth, activation
                     FROM memories WHERE kind = ?1 AND pruned = 0
                     ORDER BY created_at DESC LIMIT ?2",
                )?;
                stmt.query_map(params![k, limit as i64], |r| row_to_memory(r, now))?
                    .filter_map(|r| r.ok())
                    .collect()
            }
            None => {
                let mut stmt = conn.prepare(
                    "SELECT id, title, body, kind, created_at, accessed_at, access_count,
                            strength, warmth, activation
                     FROM memories WHERE pruned = 0
                     ORDER BY created_at DESC LIMIT ?1",
                )?;
                stmt.query_map(params![limit as i64], |r| row_to_memory(r, now))?
                    .filter_map(|r| r.ok())
                    .collect()
            }
        };
        Ok(memories)
    }

    pub fn delete(&self, id: i64) -> Result<()> {
        self.forget(id)
    }

    pub fn status(&self) -> Result<(usize, Option<i64>)> {
        let conn = self.conn.lock().unwrap();
        let count: usize =
            conn.query_row("SELECT COUNT(*) FROM memories WHERE pruned = 0", [], |r| {
                r.get::<_, i64>(0)
            })? as usize;
        let last: Option<i64> = conn
            .query_row(
                "SELECT MAX(created_at) FROM memories WHERE pruned = 0",
                [],
                |r| r.get(0),
            )
            .ok()
            .flatten();
        Ok((count, last))
    }

    pub fn graph_status(&self) -> Result<(usize, f64)> {
        let conn = self.conn.lock().unwrap();
        let edges: usize =
            conn.query_row("SELECT COUNT(*) FROM edges", [], |r| r.get::<_, i64>(0))? as usize;
        let avg_strength: f64 = conn.query_row(
            "SELECT COALESCE(AVG(strength), 0.0) FROM memories WHERE pruned = 0",
            [],
            |r| r.get(0),
        )?;
        Ok((edges, avg_strength))
    }

    // -- Markdown export (portability) ---------------------------------------

    /// Export every live memory as an individual markdown file under `dir`,
    /// with YAML frontmatter and preserved `[[wiki-links]]`. This is the
    /// ori-mnemos L0 portability layer: the vault is human-readable & editable.
    pub fn export_markdown(&self, dir: &Path) -> Result<usize> {
        std::fs::create_dir_all(dir)?;
        let conn = self.conn.lock().unwrap();
        let now = unix_now();
        let mut stmt = conn.prepare(
            "SELECT id, title, body, kind, created_at, accessed_at, access_count,
                    strength, warmth, activation
             FROM memories WHERE pruned = 0 ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| row_to_memory(r, now))?;
        let mut count = 0;
        for row in rows {
            let m = row?;
            let slug = slugify(&m.title);
            let path = dir.join(format!("{:04}-{slug}.md", m.id));
            let front = format!(
                "---
id: {id}
kind: {kind}
created_at: {created}
access_count: {ac}
                 strength: {st:.3}
warmth: {wm:.3}
activation: {act:.3}
---

# {title}

{body}
",
                id = m.id,
                kind = m.kind,
                created = m.created_at,
                ac = m.access_count,
                st = m.strength,
                wm = m.warmth,
                act = m.activation,
                title = m.title,
                body = m.body,
            );
            std::fs::write(&path, front)?;
            count += 1;
        }
        Ok(count)
    }

    // -- Helpers -------------------------------------------------------------

    fn bump_access(&self, ids: &[i64]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn.lock().unwrap();
        let now = unix_now();
        let tx = conn.transaction()?;
        for &id in ids {
            tx.execute(
                "UPDATE memories SET accessed_at = ?1, access_count = access_count + 1 WHERE id = ?2",
                params![now, id],
            )?;
            tx.execute(
                "INSERT INTO practice (memory_id, ts) VALUES (?1, ?2)",
                params![id, now],
            )?;
        }
        tx.commit()?;
        drop(conn);
        for &id in ids {
            self.recompute_activation(id)?;
        }
        Ok(())
    }
}

// -- Free functions ---------------------------------------------------------

fn row_to_memory(row: &rusqlite::Row, now: i64) -> rusqlite::Result<Memory> {
    let _ = now;
    Ok(Memory {
        id: row.get(0)?,
        title: row.get(1)?,
        body: row.get(2)?,
        kind: row.get(3)?,
        created_at: row.get(4)?,
        accessed_at: row.get(5)?,
        access_count: row.get(6)?,
        strength: row.get(7)?,
        warmth: row.get(8)?,
        activation: row.get(9)?,
    })
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// FTS5-safe OR-joined query: each token is quoted and joined with " OR ",
/// so multi-word natural-language prompts match on any token (disjunctive)
/// rather than requiring every token (the previous implicit-AND bug).
fn escape_fts_or(query: &str) -> String {
    let tokens: Vec<String> = query
        .split_whitespace()
        .flat_map(|t| t.split(|c: char| !c.is_alphanumeric()))
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{}\"", t))
        .collect();
    tokens.join(" OR ")
}

/// Escape `%` and `_` so they are treated as literals in a SQL LIKE pattern.
fn escape_like_pattern(s: &str) -> String {
    s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

/// Extract `[[Target Title]]` wiki-link destinations from markdown body.
fn extract_wiki_links(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = body.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'[' && bytes[i + 1] == b'['
            && let Some(end) = body[i + 2..].find("]]") {
                let inner = &body[i + 2..i + 2 + end];
                let title = inner.trim();
                if !title.is_empty() && !out.contains(&title.to_owned()) {
                    out.push(title.to_owned());
                }
                i = i + 2 + end + 2;
                continue;
            }
        i += 1;
    }
    out
}

fn slugify(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_owned()
}

/// Normalize a rank (0 = best) to [0,1] (1 = best).
fn rank_norm(rank: usize, total: usize) -> f64 {
    if total == 0 {
        return 0.0;
    }
    1.0 - (rank as f64 / total as f64)
}

fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

fn recency_norm(ts: i64, now: i64) -> f64 {
    let dt = (now - ts).max(0) as f64;
    // half-life of ~30 days
    (-dt / (30.0 * 86400.0)).exp()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn fresh() -> MemoryStore {
        let tmp = NamedTempFile::new().unwrap();
        let p = tmp.keep().unwrap().1;
        MemoryStore::open(&p).unwrap()
    }

    #[test]
    fn capture_and_recall() {
        let s = fresh();
        let id = s
            .capture("FTS works", "sqlite full text search is neat", "note")
            .unwrap();
        let hits = s.recall("sqlite", 5).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, id);
    }

    #[test]
    fn or_join_matches_partial() {
        // The bug that broke auto-inject: a natural-language prompt must still
        // match even if not every token appears in the document.
        let s = fresh();
        s.capture("FTS works", "sqlite full text search is neat", "note")
            .unwrap();
        assert!(
            !s.recall("tell me about sqlite search", 5)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn wiki_link_creates_edge() {
        let s = fresh();
        s.capture("Root concept", "see [[Child concept]] for details", "note")
            .unwrap();
        let child = s
            .capture("Child concept", "a derived idea", "note")
            .unwrap();
        let edges = s.edges_of(1).unwrap();
        assert!(
            edges
                .iter()
                .any(|e| e.target == child && e.relation == "derived")
        );
    }

    #[test]
    fn reflect_raises_strength() {
        let s = fresh();
        let id = s.capture("Goal", "ship the feature", "goal").unwrap();
        let before = s.recall("goal", 5).unwrap()[0].strength;
        s.reflect(id, 0.9).unwrap();
        let after = s.recall("goal", 5).unwrap()[0].strength;
        assert!(after > before, "{before} -> {after}");
    }

    #[test]
    fn cooccurrence_edges_form() {
        let s = fresh();
        let a = s.capture("Alpha", "first note about rust", "note").unwrap();
        let b = s.capture("Beta", "second note about rust", "note").unwrap();
        // both match "rust" -> recalled together -> Hebbian edge
        s.recall("rust", 5).unwrap();
        let edges = s.edges_of(a).unwrap();
        assert!(
            edges
                .iter()
                .any(|e| e.target == b && e.relation == "cooccurs")
        );
    }

    #[test]
    fn explore_walks_graph() {
        let s = fresh();
        let a = s
            .capture("Seed", "origin node for [[HopOne]]", "note")
            .unwrap();
        let b = s.capture("HopOne", "links to [[HopTwo]]", "note").unwrap();
        let c = s.capture("HopTwo", "leaf node", "note").unwrap();
        let discovered = s.explore("seed", 5).unwrap();
        // b and c are reachable from a via derived edges; a is a seed (excluded)
        let ids: Vec<i64> = discovered.iter().map(|m| m.id).collect();
        assert!(ids.contains(&b), "explore should reach HopOne: {ids:?}");
        assert!(ids.contains(&c), "explore should reach HopTwo: {ids:?}");
        assert!(!ids.contains(&a), "seed should be excluded: {ids:?}");
    }

    #[test]
    fn export_markdown_writes_files() {
        let s = fresh();
        s.capture("Exported", "body with [[Link]]", "note").unwrap();
        let dir = tempfile::TempDir::new().unwrap().keep();
        let _ = std::fs::remove_dir_all(&dir);
        let n = s.export_markdown(&dir).unwrap();
        assert_eq!(n, 1);
        assert!(dir.is_dir());
    }

    #[test]
    fn dead_memories_excluded() {
        let s = fresh();
        let id = s
            .capture("Doomed", "this will be forgotten", "note")
            .unwrap();
        s.forget(id).unwrap();
        assert!(s.recall("forgotten", 5).unwrap().is_empty());
    }
}
