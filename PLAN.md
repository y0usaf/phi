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

## Current — WS1: Substrate (`phi-host` ← `core/extensions`, 3.6k lines)

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
- [ ] **1.2 Registries + event vocabulary.** `register_tool` /
      `register_command` resurrected onto the coroutine path; emit semantics
      per `runner.ts` (sequential, registration order, per-handler
      attribution, one failure doesn't stop the rest); host-side metadata
      mirror.
- [ ] **1.3 OS bindings.** fs / process spawn / env / path — awaitable
      where pi's are async; each binding lands with an exerciser.
- [ ] **1.4 Discovery + trust gate.** `loader.ts` + `project-trust.ts`
      port: global + project `.pi/extensions` discovery, trust store
      gating project-local Lua.
- [ ] **1.5 Embedded-pack loading + acceptance.** `include_str!` packs
      through the public load path; WS1 acceptance run: an example awaits a
      host future, a hung handler is killed, untrusted `.phi/` prompts
      (prompting itself is deferred to the frontend; the substrate exposes
      the trust decision).

## Next

WS2 ai parity (layered transport/protocols/registry, catalog as data,
fixtures recorded from pi). Ladder steps get written here when the
workstream opens.
