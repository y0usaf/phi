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
        chrome_dir = { 255, 255, 255 }, -- prompt `❯` is plain text-colored
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
        -- Syntax (Monokai scopes from native-ts/color-diff/index.ts:190):
        -- code blocks render with claude-code's palette, not phi's default
        -- base16-ocean. variable/property are plain white (foreground) in
        -- upstream Monokai.
        syntax_comment = { 117, 113, 94 },
        syntax_keyword = { 249, 38, 114 },
        syntax_string = { 230, 219, 116 },
        syntax_number = { 190, 132, 255 },
        syntax_function = { 166, 226, 46 },
        syntax_type = { 166, 226, 46 },
        syntax_operator = { 249, 38, 114 },
        syntax_variable = { 255, 255, 255 },
        syntax_builtin = { 166, 226, 46 },
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
    -- A representative slice of SPINNER_VERBS (~190 upstream). Like Claude
    -- Code, one verb is picked per turn (seeded by the run start) and held
    -- for the whole run — tools included (Spinner.tsx useState initializer).
    thinking = {
        selection = "per_turn",
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
    -- `● Verb(args)` anchor (constants/figures.ts BLACK_CIRCLE — `⏺` only
    -- on darwin, `●` everywhere else — + AssistantToolUseMessage's
    -- parenthesized args), the dim `  ⎿  ` result gutter (MessageResponse,
    -- literally 5 chars), the `… +N lines (ctrl+o to expand)` marker
    -- (utils/terminal.ts — phi's expand toggle happens to be ctrl+o too),
    -- and no tinted block surface (claude-code transcripts are flat; the
    -- bullets are the only structure).
    --
    -- Second round (gap items 3–5 + message chrome): the status line is
    -- the accent-coloured asterisk spinner with the verb's shimmer sweep
    -- (GlimmerMessage: ±1-char highlight, right-to-left, uniform pulse
    -- while a tool runs) and the dim `(elapsed · ↓ tokens)` suffix; the
    -- input is the two-rule box with the `❯` prompt
    -- (PromptInput borderLeft/Right=false); user turns are the ghost `❯ `
    -- prefix over the userMessageBackground wash instead of phi's accent
    -- bar (UserPromptMessage); assistant prose leads with the `●` dot
    -- (AssistantTextMessage). In-flight calls blink the dot dim on a
    -- 600ms interval over a dim `⎿  Running…` note (ToolUseLoader +
    -- BashTool/UI's progress fallback), and the repl container borrows
    -- the tool-card shape — claude-code has no repl concept, so it
    -- renders as `● Repl` with hanging code and gutter output.
    chrome = {
        anchor_format = "{verb}({args})",
        glyph_ok = "●",
        glyph_failed = "●",
        result_prefix = "  ⎿  ",
        more_lines = "… +{n} lines (ctrl+o to expand)",
        container = "flat",
        status_indicator = "spinner",
        status_effect = "sweep",
        status_meta = true,
        input_prompt = "❯",
        input_ruled = true,
        user_prefix = "❯ ",
        user_background = true,
        assistant_prefix = "● ",
        pending_indicator = "blink_glyph",
        pending_note = "Running…",
        repl_as_tool = true,
    },

    -- ── Tool renderers: claude-code verbs ────────────────────────────────
    -- Bash / Read / Write / Update / Search / Search / List over the same
    -- prelude helpers; the chrome's anchor_format renders them `Verb(arg)`.
    -- `repl` names the container itself: with `repl_as_tool` chrome its
    -- anchor is `● Repl`, capitalized like every claude-code verb.
    tool_renderers = {
        repl = {
            verb = "Repl",
            body = { kind = "code", key = "code", lang = "repl" },
        },
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
