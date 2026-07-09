//! Embedded builtin packs — first-party behavior shipped as Lua through
//! the public extension surface (DESIGN divergence 2; locked decision:
//! embedded `.lua` via `include_str!`, loaded through the same path as
//! on-disk extensions).
//!
//! The tools pack is the `core/tools/` port (spec: pi v0.79.0), one
//! fragment file per spec module concatenated into a single chunk so the
//! fragments share the chunk-local helpers (`prelude.lua`) while parity
//! audits stay file-shaped. Fragment order is dependency order followed by
//! the spec's `createAllTools` registration order.

use phi_host::EmbeddedPack;

/// The builtin tools pack (spec: `core/tools/`). Loads under the
/// synthetic source key `<tools>`.
pub const TOOLS_PACK: EmbeddedPack = EmbeddedPack {
    name: "tools",
    source: concat!(
        include_str!("tools/prelude.lua"),
        "\n",
        include_str!("tools/truncate.lua"),
        "\n",
        include_str!("tools/path-utils.lua"),
        "\n",
        include_str!("tools/mime.lua"),
        "\n",
        include_str!("tools/file-mutation-queue.lua"),
        "\n",
        include_str!("tools/shell.lua"),
        "\n",
        include_str!("tools/output-accumulator.lua"),
        "\n",
        include_str!("tools/read.lua"),
        "\n",
        include_str!("tools/bash.lua"),
        "\n",
        include_str!("tools/edit-diff.lua"),
        "\n",
        include_str!("tools/edit.lua"),
        "\n",
        include_str!("tools/write.lua"),
        "\n",
        include_str!("tools/ls.lua"),
    ),
};
