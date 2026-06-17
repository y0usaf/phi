# phi-memory

Phi's in-house cognitive memory system: a persistent knowledge graph with
hybrid retrieval, ACT-R decay, Hebbian learning, and recursive exploration.

It is the Rust-native reimplementation of the
[ori-mnemos](https://github.com/aayoawoyemi/ori-mnemos) architecture, designed
to live inside Phi's extension runtime rather than as an external CLI bridge.

## Architecture

The system is layered, mirroring ori-mnemos's seven layers:

| Layer | Description | Status |
|-------|-------------|--------|
| L0 Storage | SQLite vault with FTS5 + markdown export | Done |
| L1 Graph & vitality | Wiki-link edges, ACT-R decay, PageRank | Done |
| L2 Fusion | BM25 + PageRank + activation + warmth + strength + recency | Done |
| L3 Dampening | Pruning below activation floor | Done |
| L4 Learning retrieval | Q-value (strength) reinforcement, Hebbian co-occurrence | Done |
| L5 Recursive exploration | Personalized PageRank walk from BM25 seeds | Done |
| L6 Adapter surface | 7 tools, 6 slash commands, 2 event hooks | Done |

## Tools (LLM-callable)

- `memory_recall(query, limit=5)` -- hybrid fusion retrieval
- `memory_capture(title, body, kind='note')` -- save with auto wiki-linking
- `memory_link(source, target, relation='related', weight=0.5)` -- explicit edge
- `memory_explore(query, limit=5)` -- PPR graph walk from BM25 seeds
- `memory_reflect(id, reward=0.5)` -- Q-learning strength reinforcement
- `memory_forget(id)` -- hard delete
- `memory_graph(id)` -- show edges of a memory

## Slash commands

- `/recall <query>` -- fusion search
- `/memories [kind]` -- list recent
- `/capture <title> -- <body>` -- save
- `/explore <query>` -- graph walk
- `/memory-status` -- vault + graph stats
- `/forget <id>` -- delete

## Auto-inject

On `BeforeAgentStart`, the extension extracts keywords from the user prompt
(filtering stopwords), runs a fusion recall, and appends a Recalled memories
block to the system prompt. The OR-joined FTS5 query ensures natural-language
prompts match on any token rather than requiring all tokens (the previous
implicit-AND bug).

## Decay

On `SessionStart`, `decay_all()` recomputes ACT-R base-level activation for
every memory from its practice history:

    B_i = ln( sum( (t_now - t_j)^(-d) ) )

Memories below the activation floor (-8.0) are soft-pruned (pruned=1) and
excluded from recall and exploration.

## Configuration

In `.phi/config.lua`:

    memory = {
      enabled = true,
      vault = '~/.phi/memory.db',  -- path to SQLite DB
      recall_limit = 5,
      auto_inject = true,
    }

## Markdown export

    store.export_markdown(Path::new('~/memories'))

Exports every live memory as an individual markdown file with YAML
frontmatter (id, kind, created_at, access_count, strength, warmth, activation)
and preserved wiki-links. This is the portability layer: the vault is
human-readable and editable.
