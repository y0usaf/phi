# phi — design & roadmap (port)

φ is **pi, ported to Rust** — an exact port of
[pi](https://github.com/badlogic/pi-mono) **v0.79.0**, pinned at `ref/pi`
commit `c5582102`, which is the spec. Where this document and the spec
disagree, the spec wins unless the divergence is one of the three below or
has a row in the locked-decisions table. This is the spine of the `port`
branch; the pre-port trees are preserved on `main` (final `fd373e0`) and
`rebuild` (final `e8cb418`) — git is the attic.

## The three divergences

Everything not listed here must look and feel **1:1 with pi**. The
side-by-side test — phi and pi v0.79.0 in adjacent terminals, same session,
indistinguishable — is the standing acceptance criterion for every
workstream.

1. **Lua, not TypeScript.** Extensions and configuration are Lua (mlua,
   vendored 5.4). Cost accepted: pi's 77 examples arrive as translations,
   not copy-paste — and each translation is a conformance test of the
   bridge. A pi example that *cannot* translate is a bridge bug, not an
   example we skip.
2. **Everything first-party is Lua on the public extension surface — no
   Rust shortcutting.** Rust is mechanism only. If a piece has product
   behavior a user could plausibly want to change, it ships as embedded
   `.lua` registered through the same API user extensions use. The Rust
   substrate provides what a Lua table cannot express: the runtime, OS and
   terminal bindings, and provider transport. Placement calls are recorded
   in the locked-decisions table; the default is Lua.
3. **The surface may exceed pi.** Where Lua + bindings can express more
   than pi's API, we grow past it — still under the exerciser rule (below).
   Superset, never dialect: pi's surface translated is the floor.

Two standing corollaries carry over from the rebuild:

- **No hook without an exerciser** — every API addition lands with an
  example extension in `examples/` in the same commit. The examples
  directory is the conformance suite and the docs.
- **No closed enums at the seam** — hosts dispatch on registered string
  keys, never match on variants.

## Doctrine conformance

| Doctrine | Status | Notes |
|---|---|---|
| 01 extension-first core | follows (strengthened) | Divergence 2 is The Rule with the escape hatch removed: first-party code is not allowed to shortcut through Rust. |
| 02 snapshot in, actions out | follows | Events in as tables, results out as tables; per-dispatch watchdog. Async seam: handlers may *await host futures* via coroutines (locked below) — still no live `&mut` host references. |
| 03 daemon + thin client | deferred | Applies to the future mux pack (post-parity, first-party era). |
| 04 declarative front, idempotent executor | n/a | No system-config surface. |
| 05 one declaration mechanism | follows | Every unit of a kind is declared via `register_*` / `on(event)`, per pi's vocabulary. |
| 06 bare core must boot | follows | Bare = substrate with zero packs: `phi --login`, `phi --list-models`, `phi "prompt"` streaming a raw completion (no tool loop — the loop is a pack). CI-checked. |
| 07 nix source of truth | follows | crane flake; `cargo fmt`/`clippy` sanctioned exceptions. |

## Locked decisions

| Decision | Choice | Rationale |
|---|---|---|
| Spec | pi v0.79.0, `ref/pi` @ `c5582102`, frozen | The port targets a snapshot and then diverges into first-party work. Upstream pi changes are cherry-picked deliberately, each recorded here — never tracked wholesale. |
| Port vs rebuild-with-own-choices | Exact port | The rebuild's own design choices (forgecode chrome, own event vocabulary) produced a tool that is not pi-polished. Pi's ~65k lines of behavior are the product; transcribing beats re-deriving. |
| From scratch vs continue | New trunk from `rebuild`, **rm everything**; salvage by resurrection, not by carrying | Spec-first beats tree-first: the workspace mirrors pi's packages so parity is auditable module-by-module. WS0 deletes all code; each workstream resurrects what it claims from the attic (`rebuild` @ `e8cb418`: OAuth/protobuf work, thread/watchdog, terminal layer, JSONL store) and reshapes it into the new layering. Nothing rides along unclaimed. |
| Extension language | Lua (divergence 1) | JS runtime considered for verbatim example reuse; rejected — embed weight, and translation-as-conformance is judged worth the cost. Final. |
| Mechanism/policy line | Rust substrate = Lua runtime (registries, event bus, watchdog), provider transport (`packages/ai` port: HTTP/SSE/OAuth/streaming), terminal mechanism (`packages/tui` port: raw mode, differential cell renderer, input decoding, components) exposed as bindings, OS bindings (fs, process spawn, env, path). Lua = tools, agent loop orchestration, slash commands, frontend/chrome, compaction policy, skills, prompt templates, themes, session naming — everything with behavior. | Divergence 2 made mechanical. A placement that puts behavior in Rust needs a row here. |
| Async seam | Lua handlers run as coroutines and may await host futures (LLM streams, subprocesses, timers) | Required for the loop and the frontend to be Lua-authored (divergence 2). Settled before the substrate lands because retrofitting it is the expensive mistake. |
| Workspace layout | `crates/phi-ai{,-types,-auth}` ← `packages/ai`; `crates/phi-agent` ← `packages/agent`; `crates/phi-tui` ← `packages/tui`; `crates/phi-app` ← `packages/coding-agent` (binary `phi`); `crates/phi-host` ← `core/extensions` (JS runtime → Lua runtime); `crates/phi-session` ← `core/session-manager` types | Crate granularity may differ from package granularity (Rust conventions); *module structure inside each crate mirrors pi's `src/`* so parity audits are `diff`-shaped — except where a crate has a structural-compression row (see `phi-ai`), in which case parity is behavioral: golden fixtures, not module diffs. |
| `phi-ai` structure | Layered, not pi-mirrored: `types → auth → transport (HTTP/SSE/retry/cancellation, written once) → protocols (wire mapping only) → registry (catalog + resolution)`; no upward deps. ~5 wire protocols as trait impls (anthropic, openai-completions, openai-responses, google, bedrock); everything else (azure, vertex, copilot, cloudflare, …) is a *variant* = catalog data row, zero Rust. The model catalog (pi's 17.5k-line `models.generated.ts`) is data — Lua tables / generated JSON via `register_provider`, never hand code. OAuth = one PKCE engine + one device-code engine + flows-as-data; irreducibly weird flows (codex exchange, copilot) stay code sharing the machinery. | Pi's per-provider file sprawl is the thing being compressed; mirroring it would forfeit the win. Parity contract instead: catalog diff vs pi's registry + recorded-fixture replay (real SSE transcripts captured from pi, replayed against phi's protocols). |
| Code standard | Robust, minimalist, modular — enforced, not aspirational: typed errors per layer (`thiserror`), no `unwrap`/`expect`/`panic!` in library crates (clippy `deny`), cross-cutting mechanisms (retry, SSE decode, truncation, cancellation) written exactly once, behavior pinned by golden-fixture tests before refactors are allowed to touch it. | "Clean" must be checkable in CI or it erodes. |
| `phi-grid` | Deleted from the workspace | Not in pi. Resurrect from the attic when the first-party mux pack returns. |
| Shipped defaults | Embedded `.lua` via `include_str!`, loaded through the public API | Divergence 2. The flake source filter must include every embedded file type. |
| Sandboxing / permissions | pi's stance: none in core, full user permissions; trust gates extension loading only (`core/project-trust` port) | Written decision so plain-Lua `os`/`io` access is a choice, not an accident. |
| Extension distribution | pi's package-manager installs from npm; phi adapts to Lua distribution | Mechanism undecided — decide when the WS7 port reaches `package-manager.ts`, not before. |
| Workstream order | Interactive vertical slice first: one protocol (anthropic) end-to-end → tools/sessions → agent loop → TUI + interactive-required coding-agent behavior → interactive frontend; provider breadth and non-interactive periphery backfill in parallel or afterward | Reordered 2026-07-08 toward a working coding agent; sharpened 2026-07-09 toward the earliest exact interactive Phi. One protocol is enough to exercise the complete product. WS5 breadth does not gate WS6–WS8, but full v0.79.0 parity still requires every workstream. Nothing is prototyped twice and no parity criterion is relaxed. |
| 1:1 acceptance | Every UI-touching workstream closes with a side-by-side session against pi v0.79.0 | "Looks and feels exactly like pi" is testable or it is nothing. |

## Architecture

Target shape (crate ← pi source, sizes are the spec's):

```
crates/
  phi-ai-types    ← packages/ai types           (mechanism)
  phi-ai-auth     ← packages/ai oauth           (mechanism; transplant, largely done)
  phi-ai          ← packages/ai (~31k)          (mechanism: all providers, streaming,
                                                 registry, overflow, images)
  phi-agent       ← packages/agent              (mechanism: loop *primitives*;
                                                 orchestration is Lua)
  phi-tui         ← packages/tui (~12k)         (mechanism: differential component
                                                 tree, editor, autocomplete, kill-ring,
                                                 undo — exposed as Lua bindings)
  phi-host        ← core/extensions (~3.6k)     (mechanism: Lua runtime, registries,
                                                 event bus, watchdog, loader, trust)
  phi-session     ← core/session-manager types  (mechanism: JSONL store)
  phi-app         ← packages/coding-agent       (thin binary: cli args, config,
                                                 mode selection; behavior is Lua)
    src/builtins/*.lua                          (policy: tools, loop, interactive
                                                 frontend, slash commands, compaction,
                                                 skills, prompt templates, themes)
examples/                                       (77 pi examples, translated —
                                                 the conformance suite)
```

## Extension surface contract

pi's surface (`core/extensions/types.ts`, 1.6k lines) translated to the Lua
bridge — the bridge is the canonical API; any Rust trait behind it is an
internal detail:

- **Read path**: plain Lua tables in (event payload + snapshot); no live
  host references cross the bridge.
- **Write path**: plain tables out (results / queued actions); validated
  against the registered key.
- **Await**: handlers are coroutines; host futures (provider streams,
  subprocesses) are awaitable.
- **Registries and events**: pi's vocabulary, translated 1:1, then grown
  (divergence 3) only alongside an exercising example.
- **Watchdog**: every host→Lua dispatch is bounded
  (`phi_host::DEFAULT_DISPATCH_TIMEOUT_MS`).
- **Frontends**: the substrate never renders. pi's interactive mode is the
  default frontend pack, authored in Lua over `phi-tui` bindings. The bare
  core's one-shot printer is the degenerate frontend.

## Roadmap

Port order follows the **interactive critical path**: WS3 → WS4 →
WS6 + the interactive-required parts of WS7 → WS8. WS5 provider breadth may
run in parallel once WS4 works, but it does not gate interactive Anthropic
use. WS9–WS10 close full-product parity after the exact default frontend is
usable. Workstream numbers preserve the package map rather than execution
order. Each workstream's acceptance is parity against `ref/pi`,
module-by-module, plus the side-by-side test where UI is involved.

- [x] **WS0 — Re-found.** Land this document; **rm everything** — all
      crates, examples, workspace files; stub flake (devshell + formatter);
      reset PLAN.md/README/AGENTS.md to the port premise. *Accept:
      `nix flake check` passes on the empty tree; docs are the only
      content.* Doctrine 06 (bare core boots) is re-established by WS2 and
      CI-checked from then on.
- [x] **WS1 — Substrate.** `phi-host` finalized: coroutine async seam, OS
      bindings (fs/proc/env/path), discovery, trust gate, embedded-pack
      loading. *Accept: an example awaits a host future; a hung handler is
      killed; untrusted `.phi/` prompts.*
- [x] **WS2 — `packages/ai` vertical slice (anthropic).** phi-ai per the
      structural-compression row, one protocol deep: types + transport
      (landed), anthropic protocol + fixture replay (landed), registry
      seam + catalog-as-data with anthropic rows, anthropic auth (PKCE
      engine + flow-as-data; resurrect the attic's OAuth work it claims),
      bare `phi` binary (doctrine 06). *Accept: anthropic fixture replay
      passes; `--list-models` matches the anthropic rows of pi's catalog;
      streaming/auth verified live where credentials exist; bare-boot CI
      check.*
- [x] **WS3 — `coding-agent` tools + sessions.** Pulled forward from
      coding-agent parity — depends only on WS1 bindings, may run parallel
      to WS2's tail: tools (read/edit/bash/write/grep/find/ls + truncation
      and output-accumulator semantics) as Lua over bindings;
      session-manager (JSONL store), settings-manager. *Accept: each
      module has a named pi counterpart with matching behavior; placement
      per the mechanism/policy line.*
- [x] **WS4 — `packages/agent` as a Lua pack.** Loop primitives in
      `phi-agent`; orchestration (`agent-loop.ts` semantics: steering,
      follow-up queue, error-as-tool-result) in embedded Lua. The Anthropic
      CLI slice is accepted by a two-request CI replay covering streamed text,
      a tool round trip, and JSONL persistence. Live credentialed one-shot
      verification remains pending. *Accept: one-shot `phi "fix the failing
      test"` edits and runs bash end to end; loop replaceable via config.*
      **The working-coding-agent milestone.**
- [ ] **WS5 — `packages/ai` breadth (parallel/backfill; not an interactive
      gate).** Remaining protocols (openai-completions,
      openai-responses, google, bedrock) as trait impls with fixture replay;
      full catalog as data; variants as catalog rows; remaining auth flows
      (codex exchange, copilot device-code) sharing the engines. Begin after
      WS4 when capacity permits; never delay WS6–WS8 for it. *Accept:
      `--list-models` matches pi's full catalog; recorded-fixture replay
      passes per protocol.*
- [ ] **WS6 — `packages/tui` parity (interactive critical path).** Port the
      component tree: differential renderer (`tui.ts`), editor,
      autocomplete, select-list, markdown, loaders, kill-ring, undo-stack,
      images — with Lua bindings for every component. Renderer, editor state,
      editor core, select-list foundation, and autocomplete foundation are
      landed; loaders, images, and full component parity remain. Markdown
      foundation landed in WS6.6. *Accept:
      component behavior matches pi's; bindings exercised by examples.*
- [ ] **WS7 — `coding-agent` remainder (interactive-required slices share
      the critical path with WS6).** resource-loader (skills,
      prompt-templates, themes), compaction, keybindings, slash commands,
      package-manager (distribution decision lands here), project-trust UI
      glue. Prioritize dependencies consumed by the next WS8 vertical slice;
      defer unrelated cleanup, not behavior. *Accept: each module has a named
      pi counterpart with matching behavior; placement per the
      mechanism/policy line.*
- [ ] **WS8 — Interactive mode (first delivery target).** pi's
      `modes/interactive` (~15k) as the default Lua frontend pack over
      `phi-tui` bindings. Build exact vertical slices and compare deterministic
      PTY/cell snapshots against pi from the first slice; no temporary Rust UI
      and no approximate frontend. *Accept: the side-by-side test — chrome,
      keybindings, overlays, footer, theming indistinguishable from pi
      v0.79.0.*
- [ ] **WS9 — Modes & periphery.** print-mode, RPC mode, export-html,
      session-picker, startup UI, cli args parity. *Accept: `phi --help`
      surface matches pi's; RPC demo example runs.*
- [ ] **WS10 — Examples as conformance.** All 77 pi examples translated to
      Lua. *Accept: every example runs or has a locked-decision row
      explaining why not; every registry and event has ≥1 exerciser.*
- [ ] **WS11 — First-party era.** Post-parity: RLM pack, mux pack (resurrect
      `phi-grid`, decide doctrine-03), surface growth beyond pi
      (divergence 3). All of it Lua on the public surface — divergence 2
      does not expire when parity lands.
