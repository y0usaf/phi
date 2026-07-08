//! phi-tui — the `packages/tui` port (spec: `ref/pi` @ `c5582102`, pi
//! v0.79.0).
//!
//! WS6 owns the component tree (differential renderer, editor,
//! autocomplete, …). The crate is seeded early with the one module the
//! WS2 bare binary needs: [`fuzzy`] (`--list-models` search).

pub mod fuzzy;
