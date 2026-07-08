# Phi (φ)

Rust coding harness, rebuilt: pi's shape (small flat-loop core, features
arrive as extensions) with a Lua-canonical extension surface. See `DESIGN.md`
for the spine — vision, locked decisions, and roadmap. The pre-rebuild tree
lives on the `main` branch.

**Phi** is named for the Greek letter `φ` (lowercase phi; uppercase `Φ`). The CLI binary is
`phi`, and Rust workspace crates use the `phi-*` prefix.

## Layout

- `crates/phi-app`: bare-core binary — login, list-models, one-shot streamed completion
- `crates/phi-ai`: provider abstraction, model registry, streaming
- `crates/phi-ai-auth`: OAuth flows and token storage/refresh
- `crates/phi-ai-types`: core LM/message/model types
- `crates/phi-tui`: terminal frontend primitives (editor, markdown, keymap)
- `crates/phi-grid`: damage-tracked ANSI cell renderer (dormant until the mux pack)
- `ref/`: local checkouts of upstream reference implementations (`pi/`, `rlm/`, …), ignored by git

Runtime state uses the `phi` application name in XDG paths, for example
`~/.local/share/phi/auth.json`.

The model catalog is entirely extension-registered: the first-party
providers (anthropic, openai, cursor, neuralwatt) live in an embedded
builtins extension (`phi.register_provider`), loaded through the same door
user extensions use — so their models are available out of the box via
`--list-models`, `--model`, and `/model` (API keys resolve from OAuth,
stored credentials, or `{PROVIDER}_API_KEY`, e.g. `NEURALWATT_API_KEY`),
and a user extension can override any of them by re-registering the name.

## Running

```bash
nix run . -- --login anthropic
nix run . -- --list-models
nix run . -- "what is 2+2?"
cargo run -p phi-app -- --help
```
