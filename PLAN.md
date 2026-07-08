# phi — port tracker

DESIGN.md is the spine (spec pin, the three divergences, locked decisions,
workstream list); this file is the ordered ladder inside the *current*
workstream only. Detail beyond this file lives in commits — if it grows past
~150 lines, it's absorbing what commits should say.

**The spec is `ref/pi` @ `c5582102` (pi v0.79.0).** Parity is judged
module-by-module against it; UI parity is judged side-by-side. The rebuild's
forgecode-visual calibration is retired: pi sets *both* the behavioral and
the visual bar now.

## Current — WS0: Re-found

- [x] **0.1 Land DESIGN.md (port premise + phi-ai compression + code
      standard).**
- [x] **0.2 rm everything.** All crates, examples, workspace files deleted;
      stub flake (devshell + formatter); README/AGENTS.md reset to the port
      premise. Salvage is by resurrection from the attic (`rebuild` @
      `e8cb418`), per the locked decision. *Accept: `nix flake check`
      passes on the empty tree.*

## Next

WS1 substrate: workspace + crane wiring return with `phi-host`; the
coroutine async seam is the first thing designed (locked decision —
retrofitting it is the expensive mistake). Then WS2 ai parity (layered
transport/protocols/registry, catalog as data, fixtures recorded from pi).
Ladder steps get written here when their workstream opens.
