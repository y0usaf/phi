---
name: next
description: Review the last completed PLAN.md step, then implement the next open item in the port roadmap. Use at the start of a fresh session to continue working through phi's plan.
---

# next

You are continuing work on phi — an **exact port of pi v0.79.0 to Rust**
(`ref/pi` @ `c5582102` is the spec). The previous session's context is
gone: DESIGN.md, PLAN.md, and git history are your memory.

## 1. Review the past step

- Read `DESIGN.md` in full — the spine: the three locked divergences (Lua
  not TS; everything first-party is Lua on the extension surface, no Rust
  shortcutting; surface superset allowed), the locked-decisions table, and
  the workstream roadmap (WS0 → WS9, dependency order).
- Read `PLAN.md` — the ladder inside the *current* workstream. The next
  open item is the first unchecked box.
- Check recent commits (`git log --oneline -10` plus the last few diffs)
  to see what actually landed. If work landed but PLAN.md wasn't updated,
  fix PLAN.md first.
- Sanity-check the tree: `git status` clean, `nix flake check` passes. If
  the last step left things broken or half-finished, finishing it IS the
  next step.

## 2. Implement the next step

- **The spec wins.** Before porting anything, read the corresponding
  module in `ref/pi/packages/` and match its behavior 1:1 — the only
  permitted divergences are the three in DESIGN.md. UI work must pass the
  side-by-side test against pi v0.79.0.
- **No Rust shortcutting.** Anything with product behavior ships as
  embedded Lua through the public extension surface. Rust is mechanism
  only (runtime, bindings, transport, terminal). A placement that puts
  behavior in Rust needs a locked-decision row.
- **No hook without an exerciser.** Every extension-API addition lands
  with an example in `examples/` in the same commit.
- **Resurrect, don't rewrite.** When a step claims code that existed
  before the re-founding (OAuth/protobuf, watchdog thread, terminal
  layer, JSONL store), pull it from the attic — `rebuild` @ `e8cb418` —
  and reshape it into the new layering. Check DESIGN.md's
  locked-decisions table before relitigating any architectural choice.
- **Code standard** (locked): typed errors per layer, no
  `unwrap`/`expect` in library crates, cross-cutting mechanisms written
  once, behavior pinned by golden fixtures before refactors touch it.
- Verify through the flake per AGENTS.md (`nix flake check`; `cargo fmt`
  / `clippy` are the sanctioned exceptions). `git add` new files before
  building — the flake only sees tracked files. If acceptance needs a
  live session, do everything headless and record what remains as a
  pending-verification note in PLAN.md.

## 3. Close the loop

- Update PLAN.md: check the item off (or note the slice landed and what
  remains). Open the next workstream's ladder in PLAN.md when the current
  one closes; update DESIGN.md roadmap checkboxes and decision rows as
  needed.
- Commit in the style of the existing history: component prefix,
  imperative, WS reference, body explains shape/why.
- End your reply with a one-paragraph handoff — what landed, what's next,
  anything the next session must know — then fold anything load-bearing
  from it into PLAN.md itself.
