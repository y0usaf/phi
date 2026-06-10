# Phi (φ)

Rust-first coding harness scaffold with an extension-first runtime and native CLI/TUI frontends.

**Phi** is named for the Greek letter `φ` (lowercase phi; uppercase `Φ`). The CLI binary is
`phi`, and Rust workspace crates use the `phi-*` prefix.

## Layout

- `crates/phi-app`: binary entrypoint
- `crates/phi-ai`: provider abstraction, model registry, streaming, and auth
- `crates/phi-rlm`: Recursive Language Model engine
- `crates/phi-repl`: REPL environment protocol and Python backend
- `crates/phi-cli`: CLI argument parsing and mode selection
- `crates/phi-tui`: native terminal frontend
- `crates/phi-runtime`: host runtime and extension registry
- `crates/phi-ext`: extension API surface
- `crates/phi-builtins`: bundled extensions that use the same host API
- `docs/`: architecture notes
- `ref/phi`: local checkout of the upstream reference implementation, ignored by git

Runtime state uses the `phi` application name in XDG paths, for example
`~/.local/share/phi/auth.json`.

## Running

```bash
cargo run -p phi-app -- --help
cargo run -p phi-app -- --list-models
nix run
nix run . -- --list-models
```
