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

- [ ] **0.1 Land DESIGN.md (port premise).** This commit.
- [ ] **0.2 Realign workspace.** Delete `phi-grid`; delete the rebuild's
      frontend (`chat.rs`, forgecode chrome in `transcript.rs`) and anything
      no port workstream claims. Keep transplants: `phi-ai*`, flake,
      `phi-host` thread/watchdog, `phi-tui` terminal layer, `phi-session`
      store. *Accept: `nix flake check` passes; bare core boots
      (`--login`, `--list-models`, one-shot raw completion).*
- [ ] **0.3 Regenerate ARCHITECTURE.md** (`scripts/gen-arch.sh`) and update
      README to the port premise.

## Next

WS1 substrate (coroutine async seam first — see the locked decision), then
WS2 ai parity. Ladder steps get written here when their workstream opens.
