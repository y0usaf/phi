# Phi Unix Philosophy Refactoring Plan

## Problem Summary

Three crates violate "do one thing well":

| Crate | Files | Lines | Concerns |
|-------|-------|-------|----------|
| phi-mux | 29 | ~7900 | 13 (rendering grid, driver, workspace, clipboard, keys, persisted state, sidebar, overlays, command line, app loop, scene composition, etc.) |
| phi-rlm | 17 | ~5500 | 9 (LM trait, prompts, parsing, config, engine loop, session continuity, provider, turn state, tests) |
| phi-session-tui | 7 | ~5900 | 1 file (mod.rs) is 4288 lines - mega-file anti-pattern |

Additionally, phi-session-tui depends on 10 other phi crates (high coupling).

---

## Phase 1: Split phi-rlm into focused crates

### Proposed crates:

| Crate | Responsibility | Source files | Depends on |
|-------|---------------|-------------|------------|
| phi-lm-core | Message types + LanguageModel trait | lib.rs (type block), handler.rs | phi-ai, serde, async-trait |
| phi-rlm-prompts | System/user prompt rendering | prompts/ (all files) | phi-lm-core |
| phi-llm-parse | Code-block extraction and prose splitting | parsing.rs | (leaf) |
| phi-rlm-config | Config, limits, events, EnvFactory | engine/config.rs | phi-lm-core, phi-repl |
| phi-rlm-engine | Single-turn completion loop + sub-calls | engine/rlm.rs, engine/state.rs, engine/subcalls.rs, engine/helpers.rs (engine-scoped), engine.rs | phi-lm-core, phi-rlm-config, phi-rlm-prompts, phi-llm-parse, phi-repl, phi-ext, phi-runtime |
| phi-rlm-session | Multi-turn persistence + compaction | engine/session.rs + session-scoped helpers | phi-rlm-engine, phi-rlm-config, phi-lm-core |
| phi-provider | Concrete ProviderLm impl | provider.rs | phi-lm-core, phi-ai |
| phi-turn | Outer harness state machine | turn.rs | phi-lm-core |
| phi-rlm (thin umbrella) | Re-exports + integration tests | lib.rs (re-exports only) | all above |

### Migration order:
1. Carve out phi-lm-core (move types + handler.rs; rename use crate:: to use phi_lm_core::)
2. Extract leaves: phi-llm-parse, phi-rlm-prompts (pure, no deps)
3. Extract phi-rlm-config (engine/config.rs alone)
4. Split engine/helpers.rs in place (engine-scoped vs session-scoped helpers)
5. Extract phi-rlm-engine (rlm.rs + state.rs + subcalls.rs + engine helpers)
6. Extract phi-rlm-session (session.rs + session helpers)
7. Move provider.rs to phi-provider, turn.rs to phi-turn
8. Reduce phi-rlm to re-exports + tests

### Dependency DAG (acyclic):

    phi-lm-core (leaf)
      |
      +-- phi-llm-parse (leaf)
      +-- phi-rlm-prompts
      +-- phi-rlm-config
      +-- phi-provider
      +-- phi-turn
      |
      phi-rlm-engine
        |
        phi-rlm-session
          |
          phi-rlm (umbrella, re-exports)

---

## Phase 2: Split phi-mux into focused crates

### Proposed crates:

| # | Crate | Responsibility | Files | Depends on |
|---|-------|---------------|-------|------------|
| 1 | phi-mux-grid | Cell-grid + ANSI rendering primitives | ui/ansi.rs, ui/cell_surface.rs, ui/layout.rs, ui/selection.rs | phi-tui |
| 2 | phi-mux-status | SessionStatus enum + SessionKey | status.rs | (leaf) |
| 3 | phi-mux-driver | Spawn and supervise embedded session task behind vt100 | driver.rs | phi-mux-status, phi-session-tui, phi-tui, vt100 |
| 4 | phi-mux-workspace | Projects/sessions model over SessionStore | workspace.rs | phi-mux-status (+ generic over SessionHandle) |
| 5 | phi-mux-clipboard | System clipboard abstraction | clipboard.rs | (leaf) |
| 6 | phi-mux-keys | Keymap + command parser | keymap.rs, command.rs | (leaf) |
| 7 | phi-mux-persisted | XDG-backed recent projects state | persisted.rs | (leaf) |
| 8 | phi-mux-sidebar | Sidebar widget | ui/sidebar.rs | phi-mux-grid, phi-mux-workspace, phi-mux-status |
| 9 | phi-mux-overlays | Modal viewers (archive, help, model, usage) | ui/overlays/*.rs | phi-mux-grid, phi-mux-status |
| 10 | phi-mux-command-line | Colon prompt input widget | command_line.rs | phi-mux-grid, phi-mux-keys |
| 11 | phi-mux (thin app) | Terminal ownership, main loop, compositing, scene | app.rs, lib.rs, ui/scene.rs | all above |

### Key refactoring: break workspace/driver cycle
- Move SessionKey into phi-mux-status (leaf)
- Make Workspace generic over handle type (Workspace<H>)
- Both driver and workspace depend on phi-mux-status, not each other

### Migration order:
1. Move SessionKey to phi-mux-status
2. Extract leaves: phi-mux-clipboard, phi-mux-persisted, phi-mux-status, phi-mux-keys
3. Extract phi-mux-grid (ui/ansi, ui/cell_surface, ui/layout, ui/selection)
4. Extract phi-mux-driver
5. Genericize Workspace<H> and extract phi-mux-workspace
6. Extract phi-mux-sidebar, phi-mux-overlays, phi-mux-command-line (parallel)
7. Final: phi-mux = app.rs + ui/scene.rs + lib.rs only

### Dependency DAG:

    phi-tui (external)
      |
      +-- phi-mux-grid (leaf)
      +-- phi-mux-keys (leaf)
      +-- phi-mux-clipboard (leaf)
      +-- phi-mux-persisted (leaf)
      +-- phi-mux-status (leaf)
      |    |
      |    phi-mux-driver
      |    phi-mux-workspace
      |      |
      |      phi-mux-sidebar
      |      phi-mux-overlays
      |      phi-mux-command-line --> phi-mux-keys
      |
      phi-mux (thin app: main loop + scene)

---

## Phase 3: Break up phi-session-tui mega-file

The 4288-line tui/mod.rs must be split into focused modules:

### Proposed module split (within phi-session-tui):
| Module | Responsibility | Approx lines |
|--------|---------------|-------------|
| tui/mod.rs (or app.rs) | Top-level TUI state machine + event loop | ~800 |
| tui/chat.rs | Chat message rendering + streaming | ~800 |
| tui/editor.rs | Input editor widget | ~600 |
| tui/navigation.rs | Scrollback, viewport, cursor management | ~500 |
| tui/events.rs | Event handling dispatch | ~500 |
| tui/composition.rs | Frame composition + layout | ~500 |
| tui/toolbar.rs | Status bar, model picker, usage display | ~500 |

### Also reduce coupling:
phi-session-tui currently depends on 10 crates. After Phase 1-2:
- phi-rlm -> phi-rlm-session (thin dep)
- phi-builtins only needed for command registration
- phi-runtime needed for host access
Consider splitting into:
- phi-session-tui-core (TUI framework, rendering)
- phi-session-tui-app (wiring, command registration, host integration)

---

## Phase 4: CLI pipe composability (text streams as universal interface)

Current gap: only the REPL<->Python boundary uses stdio JSON. Most composition is in-process trait dispatch.

### Proposed: add a CLI mode for pipe-friendly I/O

| Enhancement | Description |
|-------------|-------------|
| phi --pipe | Read JSONL from stdin, write assistant responses as JSONL to stdout |
| phi --query TEXT | One-shot query, output plain text to stdout |
| phi --json | Output structured JSON (messages, usage, metadata) |
| Exit codes | 0=success, 1=error, 2=rate-limited, 3=truncated |

This enables: `echo '{"role":"user","content":"..."}' | phi --pipe | jq .content`

---

## Summary of principle improvements

| Principle | Before | After |
|-----------|--------|-------|
| Do one thing well | phi-mux (13 concerns), phi-rlm (9 concerns) | 11 + 9 focused crates, each with 1 concern |
| Composability | In-process trait dispatch only | + CLI pipe mode for stdio JSONL |
| Small modular pieces | 3 mega-crates (29, 17, 7 files) | 20 small crates, largest ~5 files |
| Text streams | Only REPL boundary | + --pipe/--query/--json CLI modes |
| Mechanism vs policy | phi-runtime does system prompt assembly | phi-rlm-prompts owns prompt policy; phi-runtime is pure mechanism |
| Simplicity | 4288-line file, 1180-line file | Largest file < 800 lines |
| Clarity | Crate names don't map 1:1 to concerns | Each crate name = its single responsibility |
