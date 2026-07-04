-- Style pack: claude-code (WS10.2).
--
-- Imitates Claude Code's dark look through the public extension API — an
-- ordinary `phi.register_style` bundle a user could have written. Every
-- value below is read off `ref/claude-code` (utils/theme.ts darkTheme,
-- components/Spinner/utils.ts, constants/spinnerVerbs.ts); nothing in core
-- knows this pack exists. Un-expressible details are the WS10.2 gap
-- inventory in PLAN.md, not workarounds here.

phi.register_style("claude-code", {
    -- ── Theme: claude-code darkTheme ─────────────────────────────────────
    -- Roles not listed fall through to the base theme resolver (gray ramp
    -- derived from the terminal background), which matches claude-code's
    -- terminal-native text rendering.
    theme = {
        foreground = { 255, 255, 255 }, -- text
        muted = { 153, 153, 153 }, -- inactive
        heading = { 255, 255, 255 }, -- md headers are bold plain text
        border = { 136, 136, 136 }, -- promptBorder
        accent = { 215, 119, 87 }, -- claude orange (spinner glyph)
        accent_2 = { 177, 185, 249 }, -- suggestion blue-purple
        running = { 235, 159, 127 }, -- claudeShimmer (shimmer accent)
        success = { 78, 186, 101 },
        error = { 255, 107, 128 },
        warning = { 255, 193, 7 },
        tool_accent = { 215, 119, 87 }, -- claude orange tool chrome
        tool_title = { 255, 255, 255 }, -- bold-white tool names
        path = { 255, 255, 255 }, -- anchors are monochrome in claude-code
        string = { 255, 255, 255 },
        user_fg = { 255, 255, 255 },
        assistant_fg = { 255, 255, 255 },
        user_message_bg = { 55, 55, 55 }, -- userMessageBackground
        md_diff_add = { 78, 186, 101 },
        md_diff_del = { 255, 107, 128 },
    },

    -- ── Spinner: the asterisk morph ──────────────────────────────────────
    -- getDefaultCharacters() ping-ponged (SPINNER_FRAMES = chars +
    -- reversed), advanced every 120ms (SpinnerAnimationRow: time / 120).
    spinner = {
        frames = {
            "·", "✢", "✳", "✶", "✻", "✽",
            "✽", "✻", "✶", "✳", "✢", "·",
        },
        frame_interval_ms = 120,
    },

    -- ── Thinking: whimsical gerunds ──────────────────────────────────────
    -- A representative slice of SPINNER_VERBS (~190 upstream). Claude Code
    -- picks one random verb per turn and keeps it; Phi cycles on a timer —
    -- a long interval approximates the per-turn feel (gap inventory).
    thinking = {
        expressions = {
            "Accomplishing…", "Architecting…", "Baking…", "Boondoggling…",
            "Booping…", "Brewing…", "Calculating…", "Cerebrating…",
            "Churning…", "Clauding…", "Cogitating…", "Combobulating…",
            "Computing…", "Concocting…", "Contemplating…", "Cooking…",
            "Crafting…", "Crunching…", "Deciphering…", "Deliberating…",
            "Dilly-dallying…", "Discombobulating…", "Doodling…",
            "Elucidating…", "Envisioning…", "Fermenting…", "Finagling…",
            "Flummoxing…", "Forging…", "Hatching…", "Herding…",
            "Ideating…", "Incubating…", "Levitating…", "Lollygagging…",
            "Manifesting…", "Marinating…", "Moonwalking…", "Mulling…",
            "Musing…", "Noodling…", "Orchestrating…", "Percolating…",
            "Pondering…", "Pontificating…", "Prestidigitating…",
            "Razzle-dazzling…", "Recombobulating…", "Reticulating…",
            "Ruminating…", "Sautéing…", "Schlepping…", "Simmering…",
            "Spelunking…", "Sprouting…", "Synthesizing…", "Thinking…",
            "Tinkering…", "Vibing…", "Whirring…", "Whisking…",
            "Wibbling…", "Working…", "Wrangling…", "Zesting…",
        },
        tool_expression = "Working…",
        expression_interval_ms = 15000,
    },

    -- ── Chrome: gutter bullets, flat transcript, collapsed results ───────
    -- The WS10.2 gap-inventory items (1) and (2), now vocabulary: the
    -- `⏺ Verb(args)` anchor (constants/figures.ts BLACK_CIRCLE +
    -- AssistantToolUseMessage's parenthesized args), the dim `  ⎿  ` result
    -- gutter (MessageResponse, literally 5 chars), the `… +N lines
    -- (ctrl+o to expand)` marker (utils/terminal.ts — phi's expand toggle
    -- happens to be ctrl+o too), and no tinted block surface (claude-code
    -- transcripts are flat; the bullets are the only structure).
    chrome = {
        anchor_format = "{verb}({args})",
        glyph_ok = "⏺",
        glyph_failed = "⏺",
        result_prefix = "  ⎿  ",
        more_lines = "… +{n} lines (ctrl+o to expand)",
        container = "flat",
    },

    -- ── Tool renderers: claude-code verbs ────────────────────────────────
    -- Bash / Read / Write / Update / Search / Glob / List over the same
    -- prelude helpers; the chrome's anchor_format renders them `Verb(arg)`.
    tool_renderers = {
        bash = {
            verb = "Bash",
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
            verb = "Update",
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
            verb = "Glob",
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
