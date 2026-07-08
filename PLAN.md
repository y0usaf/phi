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
- [ ] **1.5 Embedded-pack loading + acceptance.** `include_str!` packs
      through the public load path; WS1 acceptance run: an example awaits a
      host future, a hung handler is killed, untrusted `.phi/` prompts
      (prompting itself is deferred to the frontend; the substrate exposes
      the trust decision).

## Next

WS2 ai parity (layered transport/protocols/registry, catalog as data,
fixtures recorded from pi). Ladder steps get written here when the
workstream opens.
