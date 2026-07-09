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
  phi_agent["phi-agent"] --> phi_ai_types["phi-ai-types"]
  phi_agent["phi-agent"] --> phi_host["phi-host"]
  phi_ai["phi-ai"] --> phi_ai_types["phi-ai-types"]
  phi_ai_auth["phi-ai-auth"] --> phi_ai_types["phi-ai-types"]
  phi_app["phi-app"] --> phi_agent["phi-agent"]
  phi_app["phi-app"] --> phi_ai["phi-ai"]
  phi_app["phi-app"] --> phi_ai_auth["phi-ai-auth"]
  phi_app["phi-app"] --> phi_ai_types["phi-ai-types"]
  phi_app["phi-app"] --> phi_host["phi-host"]
  phi_app["phi-app"] --> phi_session["phi-session"]
  phi_app["phi-app"] --> phi_tui["phi-tui"]
  phi_host["phi-host"] --> phi_ai["phi-ai"]
  phi_host["phi-host"] --> phi_ai_types["phi-ai-types"]
  phi_host["phi-host"] --> phi_session["phi-session"]
  phi_host["phi-host"] --> phi_tui["phi-tui"]
```

## Crates

| Crate | Description |
|-------|-------------|
| `phi-agent` | packages/agent port: Lua-authored agent policy pack |
| `phi-ai` | packages/ai port: transport (HTTP/SSE/retry/cancellation), protocols, registry |
| `phi-ai-auth` | packages/ai oauth port: PKCE engine, flows as data, provider registry |
| `phi-ai-types` | packages/ai type vocabulary: models, messages, content, usage, stream events |
| `phi-app` | _no description in Cargo.toml_ |
| `phi-host` | Lua extension host: coroutine async seam, event bus, watchdog |
| `phi-session` | Session manager: append-only JSONL session trees (port of core/session-manager.ts) |
| `phi-tui` | _no description in Cargo.toml_ |

## Module structure

Per-crate module/item trees (`cargo modules structure`). Functions are
omitted; types, traits, and module boundaries are the architecture.

### `phi-agent`

```
