<!-- GENERATED FILE — do not edit by hand.
     Regenerate: scripts/gen-arch.sh
     Freshness is enforced by `nix flake check` (checks.arch-fresh). -->

# Architecture map (generated)

Structural map of the workspace, extracted from the code. For rationale,
invariants, and dataflow, see DESIGN.md.

## Crate dependency graph

Internal (workspace-local) dependencies only. `A --> B` means A depends on B.

```mermaid
graph TD
```

## Crates

| Crate | Description |
|-------|-------------|
| `phi-host` | Lua extension host: coroutine async seam, event bus, watchdog |

## Module structure

Per-crate module/item trees (`cargo modules structure`). Functions are
omitted; types, traits, and module boundaries are the architecture.

### `phi-host`

```

crate phi_host
├── struct CommandInfo: pub
├── struct Host: pub
├── struct HostConfig: pub
├── struct Outcome: pub
├── struct ToolInfo: pub
├── mod api: pub(crate)
│   └── struct ResolvedCommand: pub(crate)
├── mod convert: pub(crate)
├── mod error: pub(crate)
│   └── enum HostError: pub
├── mod exec: pub(crate)
│   └── struct ExecResult: pub(crate)
├── mod os: pub(crate)
└── mod vm: pub(crate)
    ├── enum Msg: pub(crate)
    ├── struct WatchdogState: pub(self)
    └── struct Watched: pub(self)
```
