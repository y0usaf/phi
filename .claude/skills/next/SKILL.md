---
name: next
description: Review the last completed PLAN.md step, then implement the next open item in workstream order. Use at the start of a fresh session to continue working through Phi's plan.
---

You are continuing work on Phi's plan. The previous session's context is
gone — PLAN.md and git history are your memory.

## 1. Review the past step

- Read `PLAN.md` in full — it is the source of truth for direction, The
  Rule, and workstream priority. Workstream numbering *is* priority order
  (renumbered 2026-07-05): the next open item lives in the lowest-numbered
  workstream with unchecked boxes.
- Look at recent commits (`git log --oneline -15` and the diffs of the last
  few) to see what the previous session actually landed. Note: commits from
  before 2026-07-05 reference the old workstream numbers — the old→new map
  is at the top of PLAN.md's Workstreams section.
- Cross-check: does PLAN.md reflect that work? If an item was completed but
  not checked off (or a "landed as" note is missing), update PLAN.md first.
- Sanity-check the tree: `git status` is clean and the build succeeds
  before starting new work. If the last step left the build broken or
  half-finished, finishing/fixing it IS the next step.

## 2. Implement the next step

- The next step is the first unchecked item in workstream order (WS1 →
  WS12). If an item is too large for one session, take a coherent slice and
  say so in the commit and in PLAN.md.
- Enforce The Rule: if a feature can be an extension, it must be an
  extension, built through the same public API user extensions get. Reject
  cuts that hardwire extension-shaped features into core crates.
- Before touching an area, read the `ref/` sources PLAN.md's reference map
  cites for it (`ref/pi`, `ref/codex`, `ref/claude-code`, `ref/rlm`, …).
  Study the reference implementation before designing your own.
- Follow the design canon in `~/Dev/design/doctrines/` (this is a personal
  project).
- Verify through the flake, per AGENTS.md:
  `nix build .#checks.x86_64-linux.workspace-test --print-build-logs`
  (`cargo fmt` / `clippy` are the sanctioned exceptions). If the item needs
  a live session to verify, do every check that's possible headlessly and
  record what remains as a pending verification note in PLAN.md rather
  than claiming acceptance.

## 3. Close the loop

- Update PLAN.md: check the item off (or note the slice landed and what
  remains) with a "landed as" note in the style of the existing ones.
- Commit the work with a message in the style of the existing history
  (component prefix, imperative, WS reference, explains the shape/why).
- End your reply with a one-paragraph handoff: what landed, what's next,
  and anything the next session must know that isn't in PLAN.md yet —
  then fold anything load-bearing from that paragraph into PLAN.md itself.
