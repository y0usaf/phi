# phi

φ — [pi](https://github.com/badlogic/pi-mono) ported to Rust, with a
Lua-canonical extension surface.

The spec is pi **v0.79.0**, vendored at `ref/pi` (commit `c5582102`).
`DESIGN.md` is the spine: the three locked divergences from pi, the
locked-decisions table, and the port roadmap. `PLAN.md` tracks the current
workstream.

Status: **WS2 — `packages/ai` vertical slice (anthropic).** Roadmap
reordered 2026-07-08 toward the earliest working coding agent (see
DESIGN.md's workstream-order row). The previous trees are preserved on
`main` and `rebuild`; code returns workstream by workstream as ports from
the spec (and transplants from the attic).

Build/verify through Nix: `nix flake check`, `nix fmt`. See `AGENTS.md`.
