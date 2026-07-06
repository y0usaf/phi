-- Style pack: codex (WS10.3).
--
-- Imitates OpenAI Codex CLI's dark look through the public extension API —
-- an ordinary `phi.register_style` bundle a user could have written. Every
-- value below is read off `ref/codex/codex-rs/tui` (style.rs, exec_cell/render.rs,
-- chatwidget/status_surfaces.rs, history_cell/messages.rs); nothing in core
-- knows this pack exists. Un-expressible details are the WS10.3 gap inventory
-- in PLAN.md, not workarounds here.
--
-- codex is a flat-chrome harness: the braille spinner bullet, the `• Ran`
-- header, the `  └ ` tree-branch result gutter, and the `› ` user prefix over
-- a subtle background wash. Unlike claude-code the accent is ANSI cyan (bold),
-- not a custom orange — codex derives its palette from the terminal's own
-- colors (style.rs `accent_style_for` → `Color::Cyan` on dark backgrounds).

phi.register_style("codex", {
    -- ── Theme: codex ANSI-derived ────────────────────────────────────────
    -- codex doesn't ship a fixed RGB palette; it uses ratatui's `Color::Cyan`
    -- for the accent and `Color::Green`/`Color::Red` for result glyphs, all
    -- resolving to the terminal's own ANSI entries. We set only the roles
    -- codex colors distinctly from phi's defaults; the rest fall through to
    -- the base theme resolver (the terminal's palette), matching codex's
    -- terminal-native rendering. `user_message_bg` is the white@12% blend
    -- codex applies on dark backgrounds (style.rs `user_message_bg`).
    theme = {
        accent = { 0, 215, 255 }, -- ANSI 6 / cyan (codex's accent_style_for)
        accent_2 = { 0, 215, 255 },
        tool_accent = { 0, 215, 255 }, -- tool names are cyan-bold in codex
        tool_title = { 255, 255, 255 },
        running = { 0, 215, 255 },
        success = { 78, 186, 101 },
        error = { 255, 107, 128 },
        warning = { 255, 193, 7 },
        user_fg = { 255, 255, 255 },
        assistant_fg = { 255, 255, 255 },
        user_message_bg = { 43, 43, 43 }, -- white@12% over a dark bg ≈ #2b2b2b
        md_diff_add = { 78, 186, 101 },
        md_diff_del = { 255, 107, 128 },
        -- Syntax: codex uses the terminal's syntect theme; we let the base
        -- resolver handle syntax roles (unset here) so code blocks keep the
        -- user's terminal palette, as codex does.
    },

    -- ── Spinner: braille dots ────────────────────────────────────────────
    -- TERMINAL_TITLE_SPINNER_FRAMES (status_surfaces.rs:27-30): the 10-frame
    -- braille sequence advanced every 100ms. codex uses this only for the
    -- terminal title; the in-app activity marker is a `•` bullet that morphs
    -- to the braille spinner while a call is active (exec_cell/render.rs
    -- `activity_marker` → `activity_indicator`).
    spinner = {
        frames = { "⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏" },
        frame_interval_ms = 100,
    },

    -- ── Thinking: codex has no whimsical verbs ────────────────────────────
    -- codex's status line shows model/path/branch/usage, not a thinking
    -- expression. We keep phi's thinking surface but let it fall through to
    -- the base expressions (unset) so it reads as phi's default while the
    -- pack is active — the visual identity codex carries is the spinner +
    -- chrome, not the verbs.
    thinking = {},

    -- ── Chrome: tree-branch gutter, flat transcript, bullet header ───────
    -- EXEC_DISPLAY_LAYOUT (exec_cell/render.rs:740-745): command continuation
    -- `  │ `, output block `  └ ` / `    `. The activity marker is `•` (dim
    -- when idle, braille-spinner-coloured while active); success/fail are
    -- `✓`/`✗` (green/red bold). The ellipsis marker is `… +{n} lines (ctrl +
    -- t to view transcript)` (output_ellipsis_text). User messages carry the
    -- `› ` prefix (bold dim) over the user_message_bg wash (messages.rs
    -- display_lines). Assistant prose has no prefix — markdown renders bare.
    -- codex's input composer is unbordered (no two-rule box); we keep phi's
    -- stock input chrome (unset members) since codex doesn't style it.
    chrome = {
        anchor_format = "{verb} {args}",
        glyph_ok = "✓",
        glyph_failed = "✗",
        result_prefix = "  └ ",
        more_lines = "… +{n} lines (ctrl + t to view transcript)",
        container = "flat",
        status_indicator = "spinner",
        status_effect = "gradient",
        status_meta = true,
        user_prefix = "› ",
        user_background = true,
        pending_indicator = "spinner_shimmer",
        pending_note = "Waiting for tool response...",
    },

    -- ── Tool renderers: codex verbs ──────────────────────────────────────
    -- codex labels bash as "Running"/"Ran"/"You ran" (exec_cell/render.rs
    -- command_display_lines) and groups read commands under "Exploring"; the
    -- stable tool verbs we map are the ParsedCommand titles: "Search" for
    -- grep/find, "Read" for reads, "Run" for unknown. The anchor_format
    -- `{verb} {args}` renders them `Running cmd` / `Search pattern`.
    tool_renderers = {
        repl = {
            verb = "Repl",
            body = { kind = "code", key = "code", lang = "repl" },
        },
        bash = {
            verb = "Run",
            display_name = "Bash",
            category = "commands",
            body = { kind = "code", key = "cmd", lang = "bash" },
            exit_code_trailer = true,
        },
        read_file = {
            verb = "Read",
            category = "reads",
            anchor = { { key = "path", style = "path" } },
            body = { kind = "none" },
            result_lang = { kind = "path_arg", key = "path" },
        },
        write_file = {
            verb = "Write",
            category = "edits",
            anchor = { { key = "path", style = "path" } },
            body = { kind = "none" },
        },
        edit_file = {
            verb = "Edit",
            category = "edits",
            anchor = { { key = "path", style = "path" } },
            body = { kind = "none" },
        },
        grep = {
            verb = "Search",
            category = "searches",
            anchor = {
                { key = "pattern", style = "pattern" },
                { key = "path", style = "path", prefix = "in", skip_values = { "." } },
            },
            body = { kind = "none" },
        },
        find = {
            verb = "Search",
            category = "searches",
            anchor = {
                { key = "pattern", style = "pattern" },
                { key = "path", style = "path", prefix = "in", skip_values = { "." } },
            },
            body = { kind = "none" },
        },
        ls = {
            verb = "List",
            anchor = { { key = "path", style = "path" } },
            body = { kind = "none" },
        },
    },
})
