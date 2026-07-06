//! Phi's default extensions, shipped as embedded Lua source.
//!
//! Per The Rule (PLAN.md): built-in features are implemented through the same
//! public extension API user extensions use. These defaults go further — they
//! ARE user extensions, written in the same Lua a user would write, compiled
//! into the binary via `include_str!` for zero startup cost, readable in the
//! repo (`crates/phi-builtins/defaults/`) for zero-cost learning.
//!
//! The Rust wiring (`session_builder`, mux `app/mod.rs`) loads each source
//! via `phi_lua::LuaExtension::from_source` and registers it as a boxed
//! extension — the same path user `.lua` files take.
//!
//! ## What's here (Lua)
//!
//! The Rust extensions these replaced (`BuiltinCommandsExtension`,
//! `SkillsExtension`, `ApprovalsExtension`, ...) are deleted; the Lua
//! sources below are the only implementation, and the tests in
//! `crates/phi-lua/tests/defaults.rs` drive them through the same
//! `LuaExtension::from_source` path the session builder uses.
//!
//! | File | Why it's Lua |
//! |------|-------------|
//! | `commands.lua` | Pure `register_command` calls |
//! | `mux_commands.lua` | Same, `context = "mux"` |
//! | `tool_render.lua` | Declarative `register_tool_renderer` |
//! | `coding_tools.lua` | Declarative `register_tool` with `dispatch = "python"` |
//! | `visual_data.lua` | Pure literal data (`register_spinner` / `register_thinking_expression`) |
//! | `about_overlay.lua` | Simplest overlay — best tutorial |
//! | `help_overlay.lua` | WS1.6-proven, draw-ops + scroll |
//! | `skills.lua` | `phi.on("before_agent_start")` + `phi.fs` file discovery |
//! | `approvals.lua` | `phi.on("tool_call")` + `phi.ui.ask` + `phi.json`/`phi.fs` persistence |
//! | `styles/claude_code.lua` | Pure `register_style` bundle (WS10 style pack) |
//! | `styles/codex.lua` | Pure `register_style` bundle — Codex CLI imitation (WS10.3) |
//! | `styles/crush.lua` | Pure `register_style` bundle — crush Charmtone palette (WS10.3) |
//!
//! ## What stays Rust (and why)
//!
//! | Builtin | Reason |
//! |---------|--------|
//! | `CatalogExtension` | 728 KB embedded JSON via `include_str!` + `OnceLock` |
//! | `DefaultVisualExtension` (theme + text effects only) | Calls `phi_tui::generate_theme()`, `phi_tui::gradient_text()`, etc. |
//! | `usage`/`model`/`archive` overlays | Depend on `phi_session` data / complex rendering |
//! | `McpExtension` | Async tokio + reqwest + SSE |
//!
//! These honest exceptions are themselves teachable: they show what the
//! extension API can't carry (yet) and why.

/// Session slash commands (`/model`, `/help`, `/quit`, ...).
pub const COMMANDS_LUA: &str = include_str!("../defaults/commands.lua");

/// Mux colon commands (`:open`, `:archive`, `:quit`, ...).
pub const MUX_COMMANDS_LUA: &str = include_str!("../defaults/mux_commands.lua");

/// Tool display metadata for the prelude coding helpers.
pub const TOOL_RENDER_LUA: &str = include_str!("../defaults/tool_render.lua");

/// Coding tools as native tool definitions (hybrid surface, WS9.3b).
/// Registers bash/read_file/write_file/edit_file/grep/find/ls with
/// `expose = "both"` + `dispatch = "python"`.
pub const CODING_TOOLS_LUA: &str = include_str!("../defaults/coding_tools.lua");

/// Default spinner (braille dots) and thinking expressions.
/// The theme resolver and text effects stay in Rust (`DefaultVisualExtension`).
pub const VISUAL_DATA_LUA: &str = include_str!("../defaults/visual_data.lua");

/// The `phi:about` info panel (toggled by Ctrl+?).
pub const ABOUT_OVERLAY_LUA: &str = include_str!("../defaults/about_overlay.lua");

/// The `phi:help` keybinding/command reference.
pub const HELP_OVERLAY_LUA: &str = include_str!("../defaults/help_overlay.lua");

/// SKILL.md discovery + `<available_skills>` system-prompt injection.
/// Uses `phi.fs.readdir`/`phi.fs.readfile` for discovery and
/// `phi.on("before_agent_start")` for injection.
pub const SKILLS_LUA: &str = include_str!("../defaults/skills.lua");

/// Tool approval gate: evaluates tool calls against a ruleset and blocks,
/// allows, or asks the user. Uses `phi.on("tool_call")`, `phi.ui.ask`,
/// and `phi.fs` for JSON persistence of "always allow" answers.
pub const APPROVALS_LUA: &str = include_str!("../defaults/approvals.lua");

/// The `claude-code` style pack (WS10.2): Claude Code's dark look as a
/// `phi.register_style` bundle — theme, asterisk-morph spinner, gerund
/// thinking verbs, and Bash/Read/Update/Search tool verbs.
pub const STYLE_CLAUDE_CODE_LUA: &str = include_str!("../defaults/styles/claude_code.lua");
/// The `codex` style pack (WS10.3): OpenAI Codex CLI's dark look as a
/// `phi.register_style` bundle — braille spinner, tree-branch gutter,
/// flat transcript, and ANSI-cyan accent.
pub const STYLE_CODEX_LUA: &str = include_str!("../defaults/styles/codex.lua");
/// The `crush` style pack (WS10.3): Charm's crush CLI dark look as a
/// `phi.register_style` bundle — Charmtone Pantera palette, `●`/`✓`/`×`
/// tool glyphs, and `::: ` editor prompt.
pub const STYLE_CRUSH_LUA: &str = include_str!("../defaults/styles/crush.lua");
