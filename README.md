# Phi (φ)

Rust-first coding harness scaffold with an extension-first runtime and native CLI/TUI frontends.

**Phi** is named for the Greek letter `φ` (lowercase phi; uppercase `Φ`). The CLI binary is
`phi`, and Rust workspace crates use the `phi-*` prefix.

## Layout

- `crates/phi-app`: binary entrypoint
- `crates/phi-ai`: provider abstraction, model registry, streaming, and auth
- `crates/phi-rlm`: Recursive Language Model engine (core LM types, engine, harness, prompts, sessions)
- `crates/phi-repl`: REPL environment protocol and Python backend
- `crates/phi-cli`: CLI argument parsing and mode selection
- `crates/phi-tui`: native terminal frontend primitives
- `crates/phi-mux`: session multiplexer (cell-grid rendering, UI, keys, workspace)
- `crates/phi-session`: session config, transcript, usage, and JSONL store
- `crates/phi-session-tui`: TUI session loop and session builder
- `crates/phi-runtime`: host runtime and extension registry
- `crates/phi-ext`: extension API surface
- `crates/phi-builtins`: bundled extensions that use the same host API
- `crates/phi-harness`: orchestration layer between the RLM engine and frontends
- `crates/phi-lua`: Lua extension loader
- `crates/phi-memory`: persistent memory extension
- `crates/xtask`: workspace tasks (model registry generation)
- `ref/`: local checkouts of upstream reference implementations (`pi/`, `rlm/`), ignored by git

Runtime state uses the `phi` application name in XDG paths, for example
`~/.local/share/phi/auth.json`.

## Running

```bash
cargo run -p phi-app -- --help
cargo run -p phi-app -- --list-models
nix run
nix run . -- --list-models
```
