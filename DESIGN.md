# phi — design & roadmap (rebuild)

φ is a coding agent in Rust: **pi's shape** (small flat-loop harness, features
arrive as extensions) with a **Lua-canonical extension surface** generous
enough that the mux, the RLM engine, and anything a user dreams up are packs
on a minimal substrate — including the ones we ship. This document is the
spine of the `rebuild` branch, which restarts the trunk from the salvage set
below; the pre-rebuild tree is preserved on `main` (final state `fd373e0`).

## Vision

- **As small at the core as pi** (`ref/pi` is vendored as the reference):
  substrate = event loop + registries + Lua host + provider layer + session
  log. If the substrate creeps past ~5k lines, something snuck in.
- **As extensible as pi, through Lua**: pi achieves extreme customizability
  with a tiny vocabulary — six `register_*` functions, one event union, a
  `ctx` handle, and 77 example extensions that exercise every hook. We port
  that shape, not phi-ext's dozen `*Spec` types and closed enums.
- **Built via its own surface**: shipped defaults are embedded `.lua` files
  registered through the same API user extensions use. The user-facing
  surface is the tested surface by construction.
- **Provider auth is a keeper**: OAuth (Anthropic/Codex/Cursor incl. the
  protobuf/JWT work), SSE streaming, retry — `phi-ai*` survives verbatim.

## Doctrine conformance

| Doctrine | Status | Notes |
|---|---|---|
| 01 extension-first core | follows | The Rule carries over from the old PLAN.md. Sharpened: the API is defined by what a Lua table can express; a capability that can't cross the bridge doesn't ship. |
| 02 snapshot in, actions out | follows | Events in as tables, results out as tables; per-dispatch watchdog on the Lua message thread. Hot-path exception: none yet — declare here if one appears. |
| 03 daemon + thin client | deferred | Applies to the future mux pack. Open question (decide before the mux port): is the daemon the substrate's process model, or part of the pack? |
| 04 declarative front, idempotent executor | n/a | No system-config surface. |
| 05 one declaration mechanism | follows | Every unit of a kind — tool, command, provider, flag, shortcut, renderer, event handler — is declared via `register_*` / `on(event)`. No closed enums at the seam: hosts dispatch on registered string keys, never match on variants (the `UiAction` lesson). |
| 06 bare core must boot | follows | Bare = `phi --login`, `phi --list-models`, `phi "prompt"` streaming a completion, with zero extensions loaded. CI: `nix build .#phi-app` + workspace tests. |
| 07 nix source of truth | follows | crane flake; `cargo fmt`/`clippy` sanctioned exceptions. |

## Locked decisions

| Decision | Choice | Rationale |
|---|---|---|
| Rebuild vs refactor | Rebuild on a new trunk, salvage by transplant | The extension-first endpoint required rewriting the three host crates (`phi-session-tui`, `phi-mux`, `phi-rlm`) either way; greenfield skips the both-worlds tax and fixes the seam by construction. |
| v0 scope | pi in Rust: flat tool-call loop, native read/edit/bash/grep tools, interactive TUI | Mux and RLM are not v0; they return later as packs. This was already PLAN.md's stated end state ("mux and RLM as extensions on a minimal substrate"). |
| Extension language | Lua (mlua, vendored 5.4) | Lightest embed, already proven in-tree, threading model paid for. Cost accepted: no pi-ecosystem copy-paste. JS runtime revisitable if sharing demands it. |
| API definition order | Lua-first: the bridge is the canonical API; the Rust trait is an internal detail | The old bridge was Rust-trait-first and Lua silently lost fidelity (`actions: Vec::new()`, fixed overlay renderer). Inverting the order makes second-class Lua structurally impossible. |
| API growth rule | Generous surface, but **no hook without an exerciser** — every hook lands with an example extension in `examples/` in the same commit | Resolves the "no API without a consumer" debate: build ahead of demand for sharing's sake, but nothing ships unexercised (the `register_shortcut`/`register_message_renderer` lesson). The examples directory is the conformance suite and the docs. |
| Shipped defaults | Embedded `.lua`, loaded through the public API | "We build via the extension surface too", made mechanical. |
| Frontend seam | Headless substrate: the core emits the event stream and consumes input actions, never renders. A frontend is any pack bridging the two; the bare core's one-shot stdout printer is the degenerate frontend. Default frontend (WS3): inline TUI on `phi-tui`'s scrollback + live-region model, forgecode-minimal chrome, no alt screen. | Renderer swappable by extension (alt-screen, GUI, …) without touching the substrate. In-process frontends are packs; Lua-*authored* frontends wait on `phi-tui` bindings grown per the exerciser rule; out-of-process frontends (GUI/web) ride the doctrine-03 wire-protocol decision at the mux pack. |
| Sandboxing | None in core; full user permissions (pi's stance). Trust gates *extension loading* only | Written decision so plain-Lua `os`/`io` access is a choice, not an accident. |
| Salvage policy | Anything not in the keep set is deleted; resurrect from `main` history when a port needs it | Git is the attic. No dead code rides along "just in case". |

## Architecture

Keep set (after the strip; roles unchanged):

```
crates/
  phi-ai-types   core LM/message/model types            (mechanism)
  phi-ai-auth    OAuth flows, token storage/refresh     (mechanism)
  phi-ai         providers, SSE streaming, registry     (mechanism)
  phi-tui        terminal primitives: editor, markdown,
                 keymap, raw-mode terminal              (mechanism)
  phi-grid       damage-tracked ANSI cell renderer      (mechanism; dormant
                                                         until the mux pack)
  phi-app        bare-core binary: login, list-models,
                 one-shot streamed completion           (policy, minimal)
```

To be built (in roadmap order): `phi-host` (Lua runtime, registries, event
bus, watchdog), the agent loop pack, the interactive TUI. Names TBD when the
first line is written, not before.

Resurrection candidates on `main`: `phi-lua`'s `LuaMsg` thread model /
discovery / loader; `phi-session`'s JSONL store types; the Python prelude's
invariants (read-before-edit, hashline cache, spill) for the RLM pack;
`phi-mux` wholesale for the mux pack.

## Extension surface contract

pi's shape, over the Lua bridge:

- **Read path**: handlers receive plain Lua tables (event payload +
  snapshot); no live host references cross the bridge.
- **Write path**: handlers return plain tables (result / queued actions);
  the host validates against the registered key, never a variant match.
- **Registries**: `register_tool`, `register_command`, `register_provider`,
  `register_flag`, `register_shortcut`, `register_renderer` — grown only
  alongside an exercising example.
- **Events**: one flat vocabulary (pi's ~23: session/turn/message/tool
  lifecycle, `before_provider_request`, input, trust…), subscribed via
  `on(name, fn)`.
- **Watchdog**: every host→Lua dispatch is bounded (default 5000 ms —
  `phi_host::DEFAULT_DISPATCH_TIMEOUT_MS`).
- **Frontends**: the substrate never renders; a frontend subscribes to the
  same event stream and feeds input back as actions. Swapping inline TUI →
  alt-screen → GUI is a pack choice, not a core change.
- **Async seam**: settle before the host lands — whether a Lua handler can
  await host futures (LLM call, subprocess) or only queue actions.
  Retrofitting this is the expensive mistake; decide it first.

## Deferred (and why)

- **Mux pack** — after the agent works. Carries the doctrine-03 question
  (daemon as substrate vs. pack). `phi-grid` waits for it.
- **RLM pack** — the old differentiator returns as the flagship *non-default*
  pack; proves the loop-as-pack seam. Prelude invariants come back with it.
- **JS/TS extension runtime** — lost to Lua on weight; revisit only if
  extension sharing outgrows Lua.
- **Permission system / OS sandboxing** — rejected (pi's stance), unchanged
  from old PLAN.md.
- **MCP, LSP, memory, hooks-as-config** — extension territory, after the
  surface exists.
- **Old PLAN.md process** — 1,854 lines, 12 workstreams, audits of audits.
  This document stays a spine; workstream detail lives in issues/commits.

## Roadmap

- [x] **WS0 — Strip.** Delete everything outside the keep set; `phi-app`
      becomes the bare core. *Accept: `nix flake check` passes; `nix run --
      --list-models` works; `nix run -- "2+2?"` streams an answer from a
      logged-in provider; workspace is only the keep-set crates.*
- [x] **WS1 — Lua host.** `phi-host`: mlua runtime on a message thread,
      watchdog, `register_*` registries, event bus, discovery
      (`~/.config/phi/`, `.phi/`, embedded), trust gate for project-local
      extensions. *Accept: a `hello.lua` registers a command and an event
      handler; a hung handler is killed by the watchdog; an untrusted
      `.phi/` prompts; `phi-host` has no terminal dependencies (headless
      substrate per the frontend-seam decision).*
- [x] **WS2 — The loop as a pack.** Flat tool-call agent loop; native
      read/edit/bash/write tools registered through the same `register_tool`
      a user would call; one-shot mode drives it. *Accept: `phi "fix the
      failing test"` edits a file and runs bash, end to end, with the loop
      selected by config rather than hardwired.* (Live acceptance pending
      provider credit — see PLAN.md WS2.4.)
- [ ] **WS3 — First frontend pack (inline TUI).** Chat frontend on `phi-tui`
      (scrollback + bottom live region, forgecode-minimal chrome, no alt
      screen), selected like any other frontend rather than hardwired; chrome
      (footer/overlays) extension-rendered. *Accept: interactive session with
      streaming, editor, slash commands sourced only from the command
      registry.*
- [ ] **WS4 — Examples as conformance.** Port pi's examples one at a time
      (start: hello, commands, permission-gate, custom-provider,
      message-renderer, custom-footer). Each port either passes or grows the
      API. *Accept: ≥15 examples run; every registry and event has ≥1
      exerciser.*
- [ ] **WS5 — RLM pack.** *Accept: RLM selectable via config; default
      remains the flat loop; no RLM-specific branches in the substrate.*
- [ ] **WS6 — Mux pack.** Decide doctrine-03 question first. *Accept: mux
      runs as a pack; `phi-grid` is its renderer; substrate unmodified.*
