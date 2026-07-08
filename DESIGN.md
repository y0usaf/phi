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
| From scratch vs continue | New trunk from `rebuild`, salvage by transplant | Spec-first beats tree-first: the workspace mirrors pi's packages so parity is auditable module-by-module. Transplants: `phi-ai*` OAuth/SSE work, crane flake, `phi-host` thread/watchdog, `phi-tui` terminal layer. Everything else is replaced by ports; resurrect from branch history when a port wants it. |
| Extension language | Lua (divergence 1) | JS runtime considered for verbatim example reuse; rejected — embed weight, and translation-as-conformance is judged worth the cost. Final. |
| Mechanism/policy line | Rust substrate = Lua runtime (registries, event bus, watchdog), provider transport (`packages/ai` port: HTTP/SSE/OAuth/streaming), terminal mechanism (`packages/tui` port: raw mode, differential cell renderer, input decoding, components) exposed as bindings, OS bindings (fs, process spawn, env, path). Lua = tools, agent loop orchestration, slash commands, frontend/chrome, compaction policy, skills, prompt templates, themes, session naming — everything with behavior. | Divergence 2 made mechanical. A placement that puts behavior in Rust needs a row here. |
| Async seam | Lua handlers run as coroutines and may await host futures (LLM streams, subprocesses, timers) | Required for the loop and the frontend to be Lua-authored (divergence 2). Settled before the substrate lands because retrofitting it is the expensive mistake. |
| Workspace layout | `crates/phi-ai{,-types,-auth}` ← `packages/ai`; `crates/phi-agent` ← `packages/agent`; `crates/phi-tui` ← `packages/tui`; `crates/phi-app` ← `packages/coding-agent` (binary `phi`); `crates/phi-host` ← `core/extensions` (JS runtime → Lua runtime); `crates/phi-session` ← `core/session-manager` types | Crate granularity may differ from package granularity (Rust conventions); *module structure inside each crate mirrors pi's `src/`* so parity audits are `diff`-shaped. |
| `phi-grid` | Deleted from the workspace | Not in pi. Resurrect from the attic when the first-party mux pack returns. |
| Shipped defaults | Embedded `.lua` via `include_str!`, loaded through the public API | Divergence 2. The flake source filter must include every embedded file type. |
| Sandboxing / permissions | pi's stance: none in core, full user permissions; trust gates extension loading only (`core/project-trust` port) | Written decision so plain-Lua `os`/`io` access is a choice, not an accident. |
| Extension distribution | pi's package-manager installs from npm; phi adapts to Lua distribution | Mechanism undecided — decide when the WS5 port reaches `package-manager.ts`, not before. |
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

Port order = dependency order. Each workstream's acceptance is parity
against `ref/pi`, module-by-module, plus the side-by-side test where UI is
involved.

- [ ] **WS0 — Re-found.** Land this document; realign the workspace to the
      layout table (delete `phi-grid`, delete the forgecode chrome and the
      rebuild's own frontend); reset PLAN.md as the port tracker; regenerate
      ARCHITECTURE.md. *Accept: `nix flake check` passes on the stripped
      tree; bare core (doctrine 06) still boots.*
- [ ] **WS1 — Substrate.** `phi-host` finalized: coroutine async seam, OS
      bindings (fs/proc/env/path), discovery, trust gate, embedded-pack
      loading. *Accept: an example awaits a host future; a hung handler is
      killed; untrusted `.phi/` prompts.*
- [ ] **WS2 — `packages/ai` parity.** Complete the phi-ai port: every
      provider (anthropic, openai completions+responses, codex, google,
      vertex, bedrock, azure, mistral, cloudflare, copilot, images),
      overflow, model registry. *Accept: `--list-models` output matches pi's
      catalog; streaming/auth verified per provider where credentials
      exist.*
- [ ] **WS3 — `packages/tui` parity.** Port the component tree: differential
      renderer (`tui.ts`), editor, autocomplete, select-list, markdown,
      loaders, kill-ring, undo-stack, images — with Lua bindings for every
      component. *Accept: component behavior matches pi's; bindings
      exercised by examples.*
- [ ] **WS4 — `packages/agent` as a Lua pack.** Loop primitives in
      `phi-agent`; orchestration (`agent-loop.ts` semantics: steering,
      follow-up queue, error-as-tool-result) in embedded Lua. *Accept:
      one-shot `phi "fix the failing test"` edits and runs bash end to end;
      loop replaceable via config.*
- [ ] **WS5 — `coding-agent` core parity.** Tools (read/edit/bash/write/
      grep/find/ls + truncation and output-accumulator semantics) as Lua
      over bindings; session-manager, settings-manager, resource-loader
      (skills, prompt-templates, themes), compaction, keybindings,
      slash commands, project trust. *Accept: each module has a named pi
      counterpart with matching behavior; placement per the
      mechanism/policy line.*
- [ ] **WS6 — Interactive mode.** pi's `modes/interactive` (~15k) as the
      default Lua frontend pack over `phi-tui` bindings. *Accept: the
      side-by-side test — chrome, keybindings, overlays, footer, theming
      indistinguishable from pi v0.79.0.*
- [ ] **WS7 — Modes & periphery.** print-mode, RPC mode, export-html,
      session-picker, startup UI, cli args parity. *Accept: `phi --help`
      surface matches pi's; RPC demo example runs.*
- [ ] **WS8 — Examples as conformance.** All 77 pi examples translated to
      Lua. *Accept: every example runs or has a locked-decision row
      explaining why not; every registry and event has ≥1 exerciser.*
- [ ] **WS9 — First-party era.** Post-parity: RLM pack, mux pack (resurrect
      `phi-grid`, decide doctrine-03), surface growth beyond pi
      (divergence 3). All of it Lua on the public surface — divergence 2
      does not expire when parity lands.
