
## Changes Made — claude-code-style Agent Rendering for RLM Recursion

### 1. `crates/phi-rlm/src/engine.rs` — Enrich `RlmEvent` with token data
- Added `total_tokens: u64` field to `RlmEvent::SubcallEnd` and `RlmEvent::RunEnd`
- **`query()`** (llm_query path): captures `completion.usage.total_tokens` from the `Completion` returned by `complete_sub()`, threads it into the `SubcallEnd` event
- **`recurse()`** (rlm_query path): uses `completion.usage.total_tokens` from the child RLM's `RlmCompletion`
- **`fallback()`**, **`complete_run()`**, **`complete_run_since()`**: emit `total_tokens` from `usage.total_tokens` / `handler.usage().total_tokens` in `RunEnd`

### 2. `crates/phi-rlm/src/engine/config.rs` + `engine/rlm.rs` — Kept in sync
- Updated the non-compiled copies to match (future-proofing if the engine module is refactored to use them)

### 3. `crates/phi-session/src/session.rs` — Persist enriched data
- Added `total_tokens: u64` and `tool_count: usize` to `SubcallRecord` (both `#[serde(default)]` for backward compatibility with older session logs)
- Updated test fixtures to include the new fields

### 4. `crates/phi-session/src/tui.rs` — claude-code-style rendering

**New helper:**
- `agent_summary_suffix(tool_count, total_tokens, duration_secs)` — formats ` · N tool uses · M tokens · Xs`, omitting zero-valued parts

**Live progress (running):**
- `SubcallStart`: initializes `subcall_tool_counts[depth] = 0` for tracking tool calls per subcall depth
- `ToolEnd`: increments `subcall_tool_counts[depth]`, then updates the live subcall spinner row to show `· N tool uses` — matching claude-code's `AgentProgressLine` live counter
- `refresh_open_activity_rows`: spinner refresh now includes the live tool count
- Added `tool_count: usize` field to `OpenSubcallRow` struct

**Completed summary (collapsed):**
- `SubcallEnd`: drains the per-depth tool counter, persists `total_tokens` + `tool_count` into the `SubcallRecord`, and renders the label with the `agent_summary_suffix` appended (e.g., `● model ← "prompt" · 3 tool uses · 5000 tokens · 2.3s`) — matching claude-code's `Done (N tool uses · M tokens · Xs)` format
- `RunEnd` (depth > 0): **suppressed** — the separate `stop · N turns · Xs` bar is removed because the `SubcallEnd` card already carries the full summary. This avoids double-marking the same nesting level, matching claude-code's single-collapsed-summary model

**Replay (resume):**
- `push_repl_replay`: renders persisted `SubcallRecord` entries with the same `agent_summary_suffix`, so resumed sessions show the enriched summaries

**Lifecycle cleanup:**
- `subcall_tool_counts` added to `Ui` struct, initialized in `Ui::new()`, and cleared in `begin_block()`, `commit_block()`, `CodeExecStart` (depth==0), and `CodeExecEnd` (depth==0)

### 5. `crates/phi-mux/src/status.rs` — Test fix
- Updated the `RunEnd` test literal to include `total_tokens: 0`

### Results
- All 364+ tests pass (0 failures)
- Zero compiler warnings
- Backward-compatible: older session logs without `total_tokens`/`tool_count` default to 0 (via `#[serde(default)]`), and the rendering omits zero-valued parts
