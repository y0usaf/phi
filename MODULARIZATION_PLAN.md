# Phi Workspace — Modularization & Ablation Plan

## 1. Executive Summary

The `phi` Rust workspace has a clean foundation — 15 crates arranged in 7 acyclic topological layers with no circular dependencies — but degrades sharply at the top: three crates (`phi-mux` at 10,278 LOC / 27.5%, `phi-session-tui` at ~6K LOC with 9 phi-deps, and `phi-session` with a 1,272-line `session.rs`) account for 58% of total LOC. The primary problems are god-crates that conflate 5–15+ concerns each, pervasive dead code (`phi-harness` 85% unused, `phi-mux` viewport.rs/color.rs dead), a duplicate event system (`phi-harness` hooks mirroring `phi-ext`), and policy/mechanism mixing (Lua config in `phi-session`, prompt persona in `phi-rlm`, model/credential decisions in `phi-session-tui`). The plan proceeds in four risk-ordered phases: ablate dead code and dead deps first (zero-risk), then split god-files internally, then extract new crates along existing seams, and finally merge redundant crates and reshape the dependency graph.

## 2. Current Architecture Problems

1. **God-crate `phi-mux` (10,278 LOC, 27.5%)** — mixes ANSI renderer, UI layout, workspace management, and input handling; `app.rs` (1,246 lines, 25 fields, 33 methods) and `scene.rs` (1,143 lines, 6 nesting levels in `render_sidebar`) are unmanageable monoliths.
2. **God-crate `phi-session-tui` (~6K LOC, 9 phi-deps)** — conflates session construction, rendering, dispatch, login, shell escapes, and persistence; `mod.rs` (1,746L), `events.rs` (1,095L), `chat.rs` (801L) with `Ui` struct holding 52 `pub(super)` fields; `handle_rlm_event` is 466 lines.
3. **Dead public API in `phi-harness` (1,115 LOC)** — 47 exports, only 2 used by sole consumer `phi-app/src/oneshot.rs`; hooks system (394L + 120L tests), observability (171L + 40L tests), and redaction (42L) are all dead — a duplicate event system mirroring `phi-ext`'s `EventKind`/`EventHandler`.
4. **God-file `session.rs` (1,272 lines)** in `phi-session` — conflates persistence, tree navigation, conversation replay, session discovery, and 18 type definitions in one file; crate also bundles orthogonal transcript rendering + usage cost aggregation.
5. **God-file `rlm.rs` (941 lines) + `run_loop` (211 lines)** in `phi-rlm` — `run_loop` handles 12+ responsibilities inline (iteration, streaming, REPL execution, guardrails, sub-LLM dispatch, event emission, usage tracking, prose banking); `Rlm`/`RlmConfig` are god-objects with ~56 public API items.
6. **Mega-files in `phi-ai`** — `anthropic.rs` (816L), `openai_completions.rs` (724L), `openai_responses.rs` (621L) conflate serialization AND parsing; crate conflates 6+ concerns (streaming, auth/OAuth, overflow detection, cost calc, SSE decoding, HTTP retry) with 18 external deps.
7. **Overbuilt extension system** — 4 crates (`phi-ext`, `phi-runtime`, `phi-lua`, `phi-builtins`), 2,892 LOC for only bundled extensions; `phi-lua` has `SendableLua` unsafe `Send` wrapper (UB risk), dead `tokio` dep, and duplicate `dirs`/`directories` crates.
8. **Policy/mechanism mixing across workspace** — `phi-session` embeds Lua config (`mlua` contradicts "lightweight"); `phi-rlm` bakes prompt persona into engine; `phi-session-tui` makes model/credential decisions; `phi-ai` hardcodes `CLAUDE_CODE_IDENTITY` and cache-control in `anthropic.rs`.
9. **Dead code hidden by `pub` visibility** — `phi-mux` `viewport.rs` (390L, zero consumers), `color.rs` (~250L, 11 dead symbols), `phi-tui` `vt100_experiment.rs` (`#[ignore]`'d, `assert!(false)`), `phi-runtime` `futures` + `serde_json` deps (zero usage), `phi-lua` `tokio` dep (zero references).
10. **Leaky abstractions and duplicated logic** — `phi-tui` re-exports entire `crossterm` (`pub use crossterm`); `strip_ansi` duplicated with different semantics in `terminal.rs` (CSI+OSC) vs `highlight.rs` (SGR only — latent bug); `phi-ext` uses unstructured `serde_json::Value` for tool args; `phi-session` has 96 pub-item API surface with all-`pub` struct fields.

## 3. Proposed Crate Restructuring

| Old Crate | New Crate(s) | Rationale |
|---|---|---|
| `phi-ai` | `phi-ai-core` + `phi-ai-auth` + `phi-ai-types` | `phi-ai-types` removes `Auth::ChatGptOAuth` vendor coupling and decouples downstream from `reqwest`. `phi-ai-auth` isolates security-critical OAuth logic and drops 5–6 OS deps (`webbrowser`, `fd-lock`). `phi-ai-core` keeps streaming/SSE/retry. 4 dependents avoid `reqwest`/`webbrowser`. |
| `phi-mux` | `phi-mux` + `phi-grid` | Extract `phi-grid` from `phi-mux/grid/` (~3,200 LOC) — decouples the ANSI renderer/grid system from multiplexing; enables independent testing and reuse. Feature-gate `arboard`/`notify`. |
| `phi-mux` (UI) | `phi-mux-tui` (optional) | Extract entire `src/ui/` tree (`scene.rs` 1,143L, overlays, grid) into a separate TUI crate; unrelated to multiplexing core. |
| `phi-session` | `phi-session` + `phi-usage` | Extract `src/usage/` into `phi-usage` crate; `load_usage_report_from_path()` already accepts `Path` param — coupling is a thin convenience wrapper. Separates cost analytics from session management. |
| `phi-session` | `phi-session` + `phi-config` | Extract config policy (remove `mlua`) into `phi-config`; separates config resolution from storage mechanism; sheds `mlua`/`ignore`/`unicode-width` deps. |
| `phi-session-tui` | `phi-bootstrap` + `phi-session-tui` | Extract `session_builder.rs` (~410L) into `phi-bootstrap` crate; removes non-TUI logic, `mlua`/`phi-cli` deps from TUI; enables headless sessions. Breaks 9-dep coupling. |
| `phi-builtins` | `phi-meta-extension` + `phi-prelude` | `phi-meta-extension` (extension.rs scaffold) + `phi-prelude` (src/prelude/ Python coding agent); two unrelated concerns sharing no types or logic. |
| `phi-ext` | `phi-ext` + `phi-event` | Extract event domain model (`EventKind`, `EventPayload`, `LifecycleEvent`, `EventReturn`, `InputActionKind`) into `phi-event`; reduces `phi-ext` from 17 → 10 exports. |
| `phi-app` | `phi-app` + `phi-render` | Extract `render.rs`/`tool_view.rs`/`format.rs` (~430L) into `phi-render` crate; enables TUI and CLI to share one rendering codebase. |
| `phi-cli` | merge into `phi-app` | ~140 lines, single file, two deps; trivial wrapper with questionable `phi-session` dep. Merge eliminates an unnecessary indirection layer. |
| `phi-harness` | delete / merge into `phi-rlm` | 85% dead code, sole consumer uses 2 of 47 exports; harness types (`HarnessPhase`, `HarnessError`, `TurnState`) already defined in `phi-rlm` to break a dep cycle. |
| `phi-runtime` | audit for merge into `phi-ext` | Potential thin wrapper over `phi-ext`; if `AppRuntime` decomposed, remaining logic may fold into extension contract crate. |
| (new) | `phi-crypto` | De-duplicate `base64` + `sha2` usage across crates into one crypto crate. |
| (new) | `phi-paths` | De-duplicate `dirs` + `directories` → pick `directories`; centralize OS config-dir resolution. |
| (new) | `phi-fs` | Consolidate `ignore`/`notify` filesystem deps; remove `ignore` from session crates (gitignore belongs in a filesystem crate). |

## 4. Ablation Plan (Ordered by Risk: Lowest → Highest)

### Tier 1 — Zero Risk (Dead Code & Dead Deps)

1. **Delete `phi-mux` `viewport.rs` (390 lines)** — `ViewportState` & `PromptPreview` completely dead, zero consumers. `pub` visibility masks warnings.
2. **Delete dead code in `phi-mux` `color.rs` (~250 lines)** — 11 dead symbols (256-color quantization, terminal-capability detection) never wired into renderer.
3. **Delete `phi-tui` `vt100_experiment.rs`** — `#[ignore]`'d test, `assert!(false)`, tests third-party crate. Zero risk.
4. **Remove `phi-tui` dead code** — `highlight_snippet`, `StructuredBlockType`, `SyntaxColors.foreground` — dead code, trivial wrappers, premature abstractions.
5. **Remove `phi-runtime` dead deps** — `futures` (never imported), `serde_json` (no usage in source; `serde` alone suffices). Zero functional impact.
6. **Remove `phi-lua` dead `tokio` dependency** — zero references in source; entirely dead.
7. **Remove `phi-lua` dead no-op stubs** — `set_status`/`clear_status`/`notify` — `notify` writes to a table never read; stubs are literal no-ops.
8. **Remove `phi-builtins` scaffold code** — `about` command + `echo` tool (+ possibly `CoreMetaExtension`/`bundled_extensions()` entirely) — zero production value.
9. **Remove `phi-builtins` `_hashline_strip_path_noise`** — contradicts contamination detection which already rejects apply_patch syntax.
10. **Remove `phi-harness` hooks + observability + redaction (~600 lines)** — no `emit_*` method called anywhere; no subscriber outside tests; `safe_payload()` only called in tests; `SUBSCRIBERS` global static is a code smell. Zero consumer breakage (sole consumer uses 2 of 47 exports).
11. **Remove `phi-harness` dead methods + `SavePoint`** — `compact()` is a no-op stub, `navigate_tree()` misuses phase state, `SavePoint` never wired in.
12. **Remove `phi-rlm` `#[allow(dead_code)]` on `SubcallEnd.result_preview`** — explicitly dead.
13. **Remove `phi-app` `cli_strip_tool_echoes`** — stateful heuristic filtering; panel borders already differentiate tool results.
14. **Collapse `phi-lua` `dirs`/`directories` to one crate** — both resolve same OS config dir via different APIs.
15. **Remove `phi-ai` hardcoded identity** — Delete `CLAUDE_CODE_IDENTITY`; accept via `StreamOptions`; make `cache_control` configurable.

### Tier 2 — Low Risk (Mechanical Extractions, No API Change)

16. **Split `phi-builtins` `hashline.py` (958 lines)** into 5–7 modules (normalize, hashing, snapshot, view, parse, resolve, apply) — internal refactor; only `concat!` order in `mod.rs` changes.
17. **Extract `_SAFE_BUILTINS` from `phi-repl` `driver.py`** (87 entries, ~90 lines) into data file or `builtins.py` — pure config.
18. **Extract job system from `phi-repl` `driver.py`** (~100 lines) into `jobs.py` — orthogonal to namespace management.
19. **Split `phi-repl` `driver.py`** into `builtins.py`, `router.py`, `jobs.py` — 542→~300 lines.
20. **Split `xtask` `generate_models.rs`** into `models_dev.rs`, `ai_gateway.rs`, `common.rs` — pure reorg, no behavior change.
21. **Privatize `phi-ai` auth modules** — `pub mod` → `mod`; reduces API surface.
22. **Remove `eprintln!`/`.expect()` from `phi-ai`** — use structured errors + `Result<>`.
23. **Split `phi-ai` mega-files** — Separate request-builder and response-parser in `anthropic.rs` (816L), `openai_completions.rs` (724L), `openai_responses.rs` (621L).
24. **Feature-gate `pulldown-cmark` + `syntect` in `phi-tui`** — heaviest deps, only needed for markdown.

### Tier 3 — Medium Risk (Structural Reorganization)

25. **Split `phi-session` `session.rs` (1,272L)** into `store.rs`, `tree.rs`, `reconstruct.rs`, `discovery.rs`, `types.rs` — zero behavior change, structural reorganization.
26. **Make `phi-session` `lib.rs` a facade** — `mod session; pub use session::SessionStore;` reduces 96 pub-item surface.
27. **Unify `phi-session` `Config`/`CliArgs`/`Settings` field triplication** — 11+ duplicated fields across 3 structs; single source-of-truth macro.
28. **Remove `phi-session` global `USAGE_ENTRIES_CACHE`** — Replace with instance-scoped `UsageLoader` struct.
29. **Split `phi-mux` `app.rs` (1,246L)** into focused modules (input, session, commands, rendering).
30. **Split `phi-mux` `scene.rs` (1,143L)** — Extract `ScenePalette` into `src/ui/palette.rs`.
31. **Slim `phi-rlm` public API** from ~56 items to ~15 — make internal modules `pub(crate)`.
32. **Split `phi-rlm` `rlm.rs` (941L)** into 5 focused files (sinks, outcome, fallback, prompt_build, core); decompose `run_loop` into `assemble_prompt`/`invoke_model`/`process_response`/`check_limits`.
33. **Split `phi-session-tui` `handle_rlm_event` (466L)** into per-variant methods — mechanical, each arm testable.
34. **Split `phi-app` `oneshot.rs` (337L)** — move `print_events`/`print_events_ndjson` into `event_print.rs`; keep `oneshot.rs` focused on RLM config + harness.
35. **Split `phi-lua` `lib.rs`** into `discovery.rs`/`config.rs`/`loader.rs`.

### Tier 4 — High Risk (Crate Splits, Mergers, Dependency Graph Changes)

36. **Extract `phi-ai` → `phi-ai-core` + `phi-ai-auth` + `phi-ai-types`** — workspace-wide Cargo updates, trait redesign.
37. **Extract `phi-grid` from `phi-mux`** (~3,200 LOC) — broad consumer impact.
38. **Extract `phi-bootstrap` from `phi-session-tui`** — touches all 11 submodules; `use super::*` with 52 `pub(super)` fields.
39. **Decompose `phi-session-tui` `Ui` god-struct** into 7 focused sub-structs — prerequisite for all other splits in that crate.
40. **Replace `phi-lua` `SendableLua` unsafe `Send`** with mlua's `send` feature or message-passing — touches UB-prone code.
41. **Merge `phi-cli` → `phi-app`** — eliminates indirection; needs `--help` output and `trailing_var_arg` testing.
42. **Delete/merge `phi-harness`** into `phi-rlm` — `HarnessError`/`HarnessPhase` ownership migration.
43. **Extract `phi-event` from `phi-ext`** — protocol change, affects 6 dependents.
44. **Redesign `phi-ext` `EventReturn`** as explicit ExtensionHost methods — High risk, protocol change.
45. **Extract `phi-rlm` `LanguageModel` trait to `phi-ai`** — eliminates inverted dependency, changes dependency graph.

## 5. Refactoring Recommendations (Top 15, Ordered by Impact/Risk Ratio)

| # | Recommendation | Crate / File | Risk | Impact |
|---|---|---|---|---|
| R1 | Delete `phi-harness` hooks + observability + redaction (~600L dead) | `phi-harness` hooks.rs, observability.rs | Low | Eliminates duplicate event system, global static, ~600 lines |
| R2 | Delete `phi-mux` `viewport.rs` (390L) + dead `color.rs` (~250L) | `phi-mux` viewport.rs, color.rs | Zero | 640 lines removed, zero consumers |
| R3 | Split `phi-session` `session.rs` (1,272L) into 5 submodules | `phi-session` session.rs | Low | Eliminates dominant monolith; highest-value structural refactor |
| R4 | Remove dead deps: `tokio` (phi-lua), `futures`+`serde_json` (phi-runtime) | `phi-lua`, `phi-runtime` Cargo.toml | Zero | Cuts dead dependency surface |
| R5 | Split `phi-builtins` `hashline.py` (958L) into 5–7 modules | `phi-builtins` hashline.py | Low | God-file → manageable modules; only `concat!` order changes |
| R6 | Slim `phi-rlm` public API from ~56 → ~15 items | `phi-rlm` lib.rs | Low | Reduces coupling surface; `pub(crate)` internal modules |
| R7 | Make `phi-session` `lib.rs` a facade (96 → clean entry) | `phi-session` lib.rs | Low | Complements R3; tightens public boundary |
| R8 | Split `phi-ai` mega-files (anthropic 816L, completions 724L, responses 621L) | `phi-ai` anthropic.rs, openai_completions.rs, openai_responses.rs | Low | Separates request-builder from response-parser; no API change |
| R9 | Split `phi-mux` `app.rs` (1,246L) into focused modules | `phi-mux` app.rs | Medium | Prerequisite for most phi-mux refactors |
| R10 | Split `phi-rlm` `rlm.rs` (941L) + decompose `run_loop` (211L) | `phi-rlm` rlm.rs, engine/rlm.rs | Medium | Core loop touches most critical path; 12+ responsibilities → 4 functions |
| R11 | Extract `phi-grid` from `phi-mux` (~3,200 LOC) | `phi-mux` grid/ | Medium | Decouples renderer; enables independent testing |
| R12 | Extract `phi-ai` → `phi-ai-types` + `phi-ai-auth` | `phi-ai` types.rs, auth/ | Medium | 4 dependents avoid `reqwest`/`webbrowser`; isolates security logic |
| R13 | Decompose `phi-session-tui` `Ui` god-struct (52 `pub(super)` fields) into 7 sub-structs | `phi-session-tui` mod.rs | High | Prerequisite for all phi-session-tui splits; touches 11 submodules |
| R14 | Replace `phi-lua` `SendableLua` unsafe `Send` with mlua `send` feature | `phi-lua` thread.rs | High | Removes UB risk; fragile channel-boundary restructuring |
| R15 | Extract `phi-bootstrap` from `phi-session-tui` (`session_builder.rs` ~410L) | `phi-session-tui` session_builder.rs | Medium | Enables headless sessions; drops `mlua`/`phi-cli` deps from TUI |

## 6. Implementation Phases

### Phase 1: Ablation & Dead Code Removal
**Concrete Steps:**
- Delete `phi-mux` `viewport.rs` (390L) and dead symbols in `color.rs` (~250L)
- Delete `phi-tui` `vt100_experiment.rs` and dead symbols (`highlight_snippet`, `StructuredBlockType`, `SyntaxColors.foreground`)
- Remove `phi-runtime` dead deps (`futures`, `serde_json`)
- Remove `phi-lua` dead `tokio` dep, no-op stubs (`set_status`/`clear_status`/`notify`), collapse `dirs`/`directories`
- Delete `phi-harness` hooks (394L + 120L tests), observability (171L + 40L tests), redaction (42L), dead methods (`compact`, `navigate_tree`, `SavePoint`)
- Remove `phi-rlm` dead `SubcallEnd.result_preview`
- Remove `phi-app` `cli_strip_tool_echoes`
- Remove `phi-builtins` scaffold (`about`/`echo`), `_hashline_strip_path_noise`
- Remove `phi-ai` hardcoded `CLAUDE_CODE_IDENTITY`; make `cache_control` configurable

**Estimated Effort:** 2–3 person-days
**Test Gate:** `cargo build` + `cargo test` pass with no new warnings; `cargo machete` confirms no unused deps; existing `phi-app` one-shot and TUI smoke tests pass.

### Phase 2: Internal File Splits (No API Change)
**Concrete Steps:**
- Split `phi-session` `session.rs` (1,272L) → `session/{store,tree,reconstruct,discovery,types}.rs` with `mod.rs` re-exports
- Make `phi-session` `lib.rs` a facade (`mod session; pub use session::*`)
- Split `phi-builtins` `hashline.py` (958L) → 5–7 modules (normalize, hashing, snapshot, view, parse, resolve, apply)
- Split `phi-repl` `driver.py` (542L) → `builtins.py`, `router.py`, `jobs.py`
- Split `phi-ai` mega-files: separate request-builder and response-parser in `anthropic.rs` (816L), `openai_completions.rs` (724L), `openai_responses.rs` (621L)
- Split `phi-rlm` `rlm.rs` (941L) → 5 files (sinks, outcome, fallback, prompt_build, core); decompose `run_loop` (211L) → `assemble_prompt`/`invoke_model`/`process_response`/`check_limits`
- Split `phi-mux` `app.rs` (1,246L) → focused modules; extract `ScenePalette` from `scene.rs` (1,143L)
- Split `phi-app` `oneshot.rs` → `event_print.rs` + `oneshot.rs`
- Split `xtask` `generate_models.rs` → `models_dev.rs`, `ai_gateway.rs`, `common.rs`
- Slim `phi-rlm` API (~56 → ~15 items); privatize auth modules in `phi-ai`

**Estimated Effort:** 1–2 person-weeks
**Test Gate:** `cargo build` + `cargo test` + `cargo clippy` pass; no public API breakage (downstream crates compile without changes); all existing integration tests pass; line-count targets met (no file >500 lines in split crates).

### Phase 3: Crate Extractions
**Concrete Steps:**
- Extract `phi-ai-types` (move `types.rs`; removes `Auth::ChatGptOAuth` vendor coupling)
- Extract `phi-ai-auth` (move `auth/`; drops 5–6 OS deps; isolates OAuth)
- Extract `phi-grid` from `phi-mux/grid/` (~3,200 LOC)
- Extract `phi-usage` from `phi-session/src/usage/`
- Extract `phi-render` from `phi-app` (`render.rs`, `tool_view.rs`, `format.rs` ~430L)
- Extract `phi-event` from `phi-ext` (`EventKind`, `EventPayload`, `LifecycleEvent`, `EventReturn`, `InputActionKind`)
- Extract `phi-bootstrap` from `phi-session-tui` (`session_builder.rs` ~410L)
- Extract `phi-meta-extension` + `phi-prelude` from `phi-builtins`
- Decompose `phi-session-tui` `Ui` god-struct into 7 focused sub-structs (TerminalState, RunState, TranscriptState, ToolState, SessionState, UiWidgets, ViewToggles)
- Unify `phi-session` `Config`/`CliArgs`/`Settings` field triplication via macro

**Estimated Effort:** 2–3 person-weeks
**Test Gate:** `cargo build` + `cargo test` across full workspace; new crate boundaries verified (no `pub` leaks); `cargo tree` shows expected dependency reductions (`reqwest`/`webbrowser` removed from non-ai-auth crates); `phi-mux` and `phi-session-tui` integration tests pass with new crate structure.

### Phase 4: Merges & Dependency Graph Reshaping
**Concrete Steps:**
- Merge `phi-cli` → `phi-app` (replace `to_session_args()` with `#[command(flatten)]` or `From` impl)
- Delete/merge `phi-harness` into `phi-rlm` (migrate `HarnessError`/`HarnessPhase` ownership)
- Replace `phi-lua` `SendableLua` unsafe `Send` with mlua `send` feature or message-passing architecture
- Extract `phi-rlm` `LanguageModel` trait + message types to `phi-ai` (eliminates inverted dependency)
- Redesign `phi-ext` `EventReturn` as explicit `ExtensionHost` methods
- Audit `phi-runtime` for merge into `phi-ext` (if `AppRuntime` decomposed)
- Create `phi-crypto` (consolidate `base64` + `sha2`), `phi-paths` (consolidate `dirs`/`directories`), `phi-fs` (consolidate `ignore`/`notify`)
- Remove `mlua` from session crates; remove `ignore` from session crates

**Estimated Effort:** 3–4 person-weeks
**Test Gate:** Full workspace `cargo build` + `cargo test`; `cargo tree` shows no `mlua` in session crates, no `ignore` outside `phi-fs`; `phi-lua` tests pass with new concurrency model (no `unsafe`); dependency graph has no inverted edges; `phi-ext` consumers pass with new `ExtensionHost` API.

## 7. Risk Matrix

| Change | Risk Level | Impact | Mitigation |
|---|---|---|---|
| Delete `phi-harness` hooks/observability/redaction (~600L) | Low | Eliminates duplicate event system + global static | Verify zero consumers across workspace with `grep`; sole consumer uses 2 of 47 exports |
| Delete `phi-mux` `viewport.rs` (390L) + dead `color.rs` (250L) | Zero | 640 lines removed | `grep` confirms zero references; `pub` visibility was masking warnings |
| Remove dead deps (`tokio`, `futures`, `serde_json`) | Zero | Slimmer dependency tree | `cargo machete` / `cargo udeps` verification; zero source references |
| Split `phi-session` `session.rs` into 5 submodules | Low | Eliminates 1,272L monolith | Pure structural reorg; `mod.rs` re-exports preserve public API; existing tests unchanged |
| Split `phi-builtins` `hashline.py` into 5–7 modules | Low | 958L god-file → manageable | Internal refactor; only `concat!` order in `mod.rs` changes; preserve global namespace coupling |
| Slim `phi-rlm` API (~56 → ~15 items) | Low | Reduces coupling surface | Audit downstream `phi-app`/`phi-session-tui` usage; `pub(crate)` internal modules |
| Split `phi-ai` mega-files (816L/724L/621L) | Low | Separates serialization from parsing | No API change; internal split; request-builder and response-parser become separate modules |
| Split `phi-mux` `app.rs` (1,246L) into modules | Medium | Prerequisite for phi-mux refactors | Preserve `App` as facade delegating to sub-modules; test incrementally; 25 fields → grouped sub-structs |
| Split `phi-rlm` `rlm.rs` (941L) + decompose `run_loop` | Medium | Core loop: 12+ responsibilities → 4 functions | Touches most critical path; each extracted function independently testable; preserve behavior with golden tests |
| Extract `phi-grid` from `phi-mux` (~3,200 LOC) | Medium | Decouples renderer | Broad consumer impact in `phi-mux`; adapter traits for `vt100` leaking into render layers; test compositing separately |
| Extract `phi-ai` → `phi-ai-types` + `phi-ai-auth` | Medium | 4 dependents avoid `reqwest`/`webbrowser` | Workspace-wide Cargo.toml updates; `Auth::ChatGptOAuth` vendor coupling removed; test auth flows separately |
| Extract `phi-usage` from `phi-session` | Medium | Separates cost analytics | `load_usage_report_from_path()` already accepts `Path`; caller updates in `phi-mux`; thin coupling |
| Extract `phi-bootstrap` from `phi-session-tui` | Medium | Enables headless sessions; drops `mlua`/`phi-cli` deps | `session_builder.rs` ~410L; test headless session construction independently |
| Decompose `phi-session-tui` `Ui` god-struct (52 `pub(super)` fields) | High | Prerequisite for all phi-session-tui splits | All 11 submodules use `use super::*`; do incrementally; extract one sub-struct at a time with test gates |
| Replace `phi-lua` `SendableLua` unsafe `Send` | High | Removes UB risk | Fragile channel-boundary restructuring; use mlua `send` feature or message-passing; test with stress/timeout scenarios |
| Merge `phi-cli` → `phi-app` | Low–Medium | Eliminates trivial wrapper indirection | ~140 lines; test `--help` output and `trailing_var_arg` behavior; verify no downstream depends on `phi-cli` directly |
| Delete/merge `phi-harness` into `phi-rlm` | Medium | Removes unnecessary layer | `HarnessError`/`HarnessPhase` ownership migration; verify `phi-app/src/oneshot.rs` still compiles |
| Extract `phi-event` from `phi-ext` | Medium | Reduces `phi-ext` 17 → 10 exports | 6 dependents need Cargo updates; test event dispatch round-trip |
| Redesign `phi-ext` `EventReturn` as explicit host APIs | High | Separates policy from mechanism | Protocol change; all extension consumers affected; design new `ExtensionHost` trait first; provide migration shim |
| Extract `phi-rlm` `LanguageModel` trait to `phi-ai` | High | Eliminates inverted dependency | Changes dependency graph; `lm_core` types move across crates; coordinate with `phi-ai-types` extraction |
| Unify `phi-session` config struct triplication | Medium | Cuts 5 edit-sites to 1 per setting | Macro-based struct generation; brittle but working pattern; test `Settings::resolve` and `take!` macro exhaustively |
| Replace `serde_json::Value` tool args with typed `ToolCallView` | Medium | Eliminates raw JSON escape hatch | Removes `serde_json` dep from `phi-ext`/`phi-session-tui`; add typed deserialization with fallback |
| Feature-gate heavy deps (`syntect`, `webbrowser`, `arboard`, `mlua`) | Low | Reduces default dep surface | Feature flags already optional in some crates; test both feature-on and feature-off builds |
