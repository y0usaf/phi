# AGENTS.md

Guide for AI coding agents working in the `phi` repository.

## Direction — read DESIGN.md first

`DESIGN.md` at the repo root is the source of truth: phi is an **exact port
of pi v0.79.0** (`ref/pi` @ `c5582102` is the spec) with exactly three
locked divergences — Lua instead of TypeScript, everything first-party
authored as Lua on the public extension surface (no Rust shortcutting), and
a surface that may grow past pi's. Everything else must look and feel 1:1
with pi; the side-by-side test is the standing acceptance criterion.

Standing corollaries: **no hook without an exerciser** (every API addition
lands with an example extension in the same commit) and **no closed enums
at the seam** (hosts dispatch on registered string keys, never match on
variants). Check `DESIGN.md`'s locked-decisions table before relitigating
an architectural choice; `PLAN.md` is the ladder inside the current
workstream.

## State of the tree

WS2 in progress: `packages/ai` vertical slice (anthropic) — the roadmap
was reordered 2026-07-08 to reach a working coding agent first (see the
workstream-order locked row in DESIGN.md). Workspace under `crates/`,
built with crane; regenerate `ARCHITECTURE.md` via `scripts/gen-arch.sh`
after structural changes.

The previous trees are preserved on `main` (final `fd373e0`) and `rebuild`
(final `e8cb418`); git is the attic. Resurrect from branch history when a
port claims something (e.g. the OAuth/protobuf work in `rebuild`'s
`phi-ai*`), never carry code "just in case".

## Build System — Nix is the Source of Truth

Build and verify through the flake, not the language's native tool.

| Goal | Command |
|------|---------|
| Run all checks | `nix flake check` |
| Format Nix | `nix fmt` |
| Format Rust (when code exists) | `cargo fmt` — sanctioned exception |
| Lint Rust (when code exists) | `cargo clippy --workspace` — sanctioned exception |

**Gotcha:** the flake only sees git-tracked files. `git add` any newly
created file before `nix build`, or the build fails.

## Code standard (locked in DESIGN.md)

- Rust edition 2024; typed errors per layer (`thiserror`); no
  `unwrap`/`expect`/`panic!` in library crates (clippy `deny`).
- Cross-cutting mechanisms (retry, SSE decode, truncation, cancellation)
  are written exactly once.
- Behavior is pinned by golden-fixture tests (recorded from pi) before
  refactors may touch it.
- Non-Rust files that are `include_str!`'d (`.lua`, `.json`) must be
  included in the flake's source filter.

## Checklist

1. `cargo fmt` (when code exists)
2. `nix flake check`
3. Update `PLAN.md` checkboxes / `DESIGN.md` decision rows as work lands
