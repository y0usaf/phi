# phi — port tracker

DESIGN.md is the spine (spec pin, the three divergences, locked decisions,
workstream list); this file is the ordered ladder inside the *current*
workstream only. Detail beyond this file lives in commits — if it grows past
~150 lines, it's absorbing what commits should say.

**The spec is `ref/pi` @ `c5582102` (pi v0.79.0).** Parity is judged
module-by-module against it; UI parity is judged side-by-side. The rebuild's
forgecode-visual calibration is retired: pi sets *both* the behavioral and
the visual bar now.

## Done — WS0: Re-found

- [x] **0.1 Land DESIGN.md (port premise + phi-ai compression + code
      standard).**
- [x] **0.2 rm everything.** All crates, examples, workspace files deleted;
      stub flake (devshell + formatter); README/AGENTS.md reset to the port
      premise. Salvage is by resurrection from the attic (`rebuild` @
      `e8cb418`), per the locked decision. *Accept: `nix flake check`
      passes on the empty tree.* ✓ verified.

## Done — WS1: Substrate (`phi-host` ← `core/extensions`, 3.6k lines)

Seam decisions made at 1.1 (recorded here, exercised by tests + example):
extension chunks are called with the API table as their single argument
(`local pi = ...` — the Lua mirror of pi's `export default function (pi)`);
the API table is named `pi`, matching the spec's vocabulary so example
translations stay literal; handlers run as Lua coroutines driven as futures
on a dedicated VM thread (current-thread tokio); the watchdog budget counts
*Lua execution slices only* — time suspended awaiting a host future is free.

- [x] **1.1 Workspace + async seam.** Cargo workspace + crane flake return;
      `crates/phi-host` with coroutine dispatch, JSON⇄Lua boundary
      (resurrected `convert.rs`), instruction-hook watchdog adapted to
      slices, `pi.on(event, fn)` + `pi.sleep(ms)` (first awaitable host
      future). *Accept: tests — a handler awaits `pi.sleep` and returns; a
      busy-loop handler is killed; a sleep longer than the budget survives
      (await time is free); exerciser `examples/extensions/await-demo.lua`
      loads and runs.* ✓ all landed; `nix flake check` = workspace-test +
      workspace-clippy (`-D warnings`, unwrap/expect/panic denied) +
      arch-fresh. Watchdog mechanics: global instruction hook + slice
      metering in `vm.rs` (`Watched` poll wrapper); dispatch is sequential
      per message, matching `runner.ts` emit order.
- [x] **1.2 Registries + event vocabulary.** `pi.register_tool` /
      `pi.register_command` on the coroutine path with `loader.ts`
      semantics (per-extension maps, re-registration replaces in place);
      host-side metadata mirror: `Host::tools()` (first registration per
      name wins across extensions, functions stripped from meta),
      `Host::commands()` (collision → `name:N` invocation names per
      `resolveRegisteredCommands`), `Host::call_tool` / `Host::call_command`
      under the watchdog. Exercisers: `hello.lua` (translation of pi's
      `hello.ts`), `command-demo.lua`. ✓ emit semantics were already per
      `runner.ts` from 1.1. *Pending:* tool `execute` receives
      `(tool_call_id, params)` today — the spec's remaining params
      (signal, on_update, ctx) land with their host mechanisms
      (cancellation, update streaming, context binding) in later steps;
      command handlers likewise run as `handler(args)` until ctx exists.
- [x] **1.3 OS bindings.** `pi.exec(command, args?, opts?)` — 1:1 port of
      the spec's `core/exec.ts` (`exec.rs`): never throws, spawn failure →
      `code=1`, timeout → SIGTERM then SIGKILL+5s with `killed=true`,
      post-exit stdio grace (`EXIT_STDIO_GRACE_MS`), cwd defaults to
      `HostConfig.cwd` (spec: the loader's injected cwd). `pi.fs.*` (async
      tokio — await is watchdog-free; errors throw, `pcall` = `try/catch`),
      `pi.path.*` (Node posix semantics, pinned by unit tests), `pi.env`
      (read-only view; mutation raises — revisit if a translation needs
      env writes), `pi.cwd()` — ambient Node in pi, explicit bindings
      under divergence 1 (`os.rs`). Exercisers: `exec-demo.lua` (the non-UI
      path of pi's dirty-repo-guard.ts — full translation when ctx lands),
      `os-demo.lua`. *Pending:* `exec` `options.signal` lands with the
      cancellation mechanism (same step as tool `signal`).
- [x] **1.4 Discovery + trust gate.** `loader.ts` discovery half
      (`discover.rs`: direct `.lua` files, subdir `init.lua`, one level,
      project `.phi/extensions` → global `~/.phi/agent/extensions` →
      configured paths, dedup; project dir gated by `project_trusted`) +
      `trust-manager.ts`/`project-trust.ts` port (`trust.rs`:
      `ProjectTrustStore` on `trust.json` with mkdir lock,
      nearest-ancestor lookup, options, `has_project_trust_inputs`,
      `resolve_project_trusted` → `Decided(bool)` | `Ask` — prompting is
      the frontend's; headless `Ask` = untrusted). `Host::load_file` /
      `load_extensions` collect per-path errors (spec `loadExtensions`).
      `project_trust` event folded via `trust_event_result` (first yes/no
      wins, undecided falls through). Exerciser:
      `project-trust.lua` (non-UI path of pi's example; UI branches when
      ctx lands). *Pending:* spec's `package.json` `pi`-manifest entry
      rule → WS5 package-manager decision (locked row); spec's
      `emitProjectTrustEvent` stops dispatch at the first yes/no —
      `Host::emit` runs all handlers and the fold scans; revisit with the
      ctx binding if side-effect order becomes observable.
- [x] **1.5 Embedded-pack loading + acceptance.** `EmbeddedPack` +
      `Host::load_embedded` — `include_str!` chunks through the public
      load path under synthetic `<name>` keys (spec: loader.ts `<inline>`
      convention), per-pack errors in the `LoadReport` shape; one shared
      registry with on-disk extensions (first-wins verified). WS1
      acceptance run pinned as `tests/acceptance.rs` (one end-to-end
      scenario: untrusted `.phi/` → `Ask` + discovery skip, embedded +
      discovered load, await works, hang killed, survivor runs). ✓
      `nix flake check`.

Carried out of WS1 (land with their host mechanisms): tool `execute`
still receives `(tool_call_id, params)` — signal/on_update/ctx come with
cancellation, update streaming, and the ctx binding (WS4/WS5); `pi.exec`
`options.signal` same step; command handlers run as `handler(args)` until
ctx exists; `package.json`-manifest analogue → WS5 package-manager
decision; `emit` runs all `project_trust` handlers where the spec stops at
the first yes/no — revisit with ctx if side-effect order becomes
observable.

## Current — WS2: `packages/ai` parity (`phi-ai*`, ~31k lines compressed)

Structure is the locked `phi-ai` row: layered `types → auth → transport →
protocols → registry`, no upward deps; parity is behavioral (fixtures),
not module diffs. Steps refine as the spec modules are read; doctrine 06
(bare core boots) is re-established at the end of this workstream and
CI-checked from then on.

- [x] **2.1 `phi-ai-types`.** Port `packages/ai` core types (model,
      messages, content, usage/cost, stream events) as the shared
      vocabulary crate; serde shapes pinned by fixtures from pi.
      ✓ `types.rs` ← `types.ts` (open-string Api/Provider + `KNOWN_*`
      lists; TS literal `type`/`role` discriminants as single-variant
      marker enums so unions stay untagged *and* validated — serde's
      struct-level `tag` does not check on deserialize; `Model.compat`
      stays raw JSON, decoded per-family via `Model::compat<T>()`);
      `diagnostics.rs` ← `utils/diagnostics.ts`; `models.rs` ← the pure
      half of `models.ts` (calculate_cost, get/clamp thinking levels,
      models_are_equal — registry half is 2.4). Fixtures: real pi-recorded
      session messages (recorder was v0.80.3 — `usage.reasoning` stripped,
      not in the 0.79.0 spec; provenance in `tests/fixtures/README.md`) +
      verbatim `models.generated.ts` rows + hand-built event/images/
      diagnostics shapes; round-trip = Value equality with numbers
      normalized to f64. Options families (StreamOptions etc.) carry
      AbortSignal/callbacks → land with transport (2.2); event-stream
      mechanism likewise 2.2.
- [ ] **2.2 Transport, written once.** HTTP + SSE decode + retry +
      cancellation in one module; no protocol knowledge.
- [ ] **2.3 Protocols as trait impls.** anthropic first, with the
      recorded-fixture replay harness (real SSE transcripts captured from
      pi); then openai-completions, openai-responses, google, bedrock.
- [ ] **2.4 Registry + catalog as data.** Catalog generated from pi's
      `models.generated.ts` as data (JSON/Lua tables, never hand code);
      variants (azure, vertex, copilot, …) as catalog rows;
      `pi.register_provider` binding + exerciser example.
- [ ] **2.5 Auth.** `phi-ai-auth`: one PKCE engine + one device-code
      engine + flows-as-data; resurrect the attic's OAuth/protobuf work
      (`rebuild` @ `e8cb418`, `phi-ai*`) function-by-function; codex/
      copilot stay code sharing the machinery.
- [ ] **2.6 Bare core boots (doctrine 06).** Thin `phi` binary:
      `--login`, `--list-models`, `phi "prompt"` streaming a raw
      completion (no tool loop). *Accept: `--list-models` matches pi's
      catalog; fixture replay per protocol; live streaming/auth where
      credentials exist; bare-boot CI check.*
