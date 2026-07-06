-- Style pack: crush (WS10.3).
--
-- Imitates Charm's crush CLI dark look through the public extension API —
-- an ordinary `phi.register_style` bundle a user could have written. Every
-- value below is read off `ref/crush/internal/ui/styles` (themes.go's
-- CharmtonePantera, quickstyle.go, styles.go glyph constants); nothing in
-- core knows this pack exists. Un-expressible details are the WS10.3 gap
-- inventory in PLAN.md, not workarounds here.
--
-- crush is a card-chrome harness with the Charmtone Pantera palette: the
-- `●`/`✓`/`×` tool status glyphs, info-coloured tool names, the
-- `::: ` editor prompt, and left-bordered message turns. The Charmtone RGB
-- values are verbatim from `github.com/charmbracelet/x/exp/charmtone`.

phi.register_style("crush", {
    -- ── Theme: Charmtone Pantera ─────────────────────────────────────────
    -- Verbatim RGB from charmtone.go (see ref/crush theme tables). Roles
    -- not listed fall through to the base resolver (gray ramp from the
    -- terminal bg), matching crush's neutral neutrals.
    theme = {
        foreground = { 223, 219, 221 }, -- Ash
        muted = { 133, 131, 146 }, -- Squid (fgMoreSubtle)
        heading = { 191, 188, 200 }, -- Smoke (fgSubtle)
        border = { 58, 57, 67 }, -- Charcoal (separator / bgLessVisible)
        accent = { 107, 80, 255 }, -- Charple (primary — purple)
        accent_2 = { 255, 96, 255 }, -- Dolly (secondary — magenta)
        running = { 232, 255, 39 }, -- Citron (busy)
        success = { 0, 255, 178 }, -- Julep
        warning = { 245, 239, 52 }, -- Mustard
        error = { 235, 66, 104 }, -- Sriracha
        tool_accent = { 0, 164, 255 }, -- Malibu (info — tool names)
        tool_title = { 223, 219, 221 }, -- Ash
        tool_output = { 133, 131, 146 }, -- Squid
        subcall_accent = { 255, 96, 255 }, -- Dolly
        path = { 0, 164, 255 }, -- Malibu
        string = { 0, 255, 178 }, -- Julep
        number = { 245, 239, 52 }, -- Mustard
        user_fg = { 223, 219, 221 }, -- Ash
        assistant_fg = { 223, 219, 221 }, -- Ash
        user_message_bg = { 45, 44, 53 }, -- BBQ (bgLeastVisible)
        -- Charmtone syntax scopes (Chroma config in quickstyle.go):
        syntax_comment = { 96, 95, 107 }, -- Oyster (fgMostSubtle)
        syntax_keyword = { 0, 164, 255 }, -- Malibu (info)
        syntax_string = { 191, 151, 111 }, -- Cumin (LiteralString)
        syntax_number = { 0, 255, 178 }, -- Julep (LiteralNumber = success)
        syntax_function = { 18, 199, 143 }, -- Guac (successMostSubtle — NameFunction)
        syntax_type = { 114, 114, 255 }, -- Guppy (KeywordType)
        syntax_operator = { 255, 127, 144 }, -- Salmon (Operator)
        syntax_variable = { 191, 188, 200 }, -- Smoke (Name)
        syntax_builtin = { 255, 121, 208 }, -- Cheeky (NameBuiltin)
        syntax_punctuation = { 232, 254, 150 }, -- Zest (warningSubtle — Punctuation)
        md_diff_add = { 0, 255, 178 }, -- Julep
        md_diff_del = { 255, 87, 125 }, -- Coral (destructive)
    },

    -- ── Spinner: the working gradient ─────────────────────────────────────
    -- crush's working indicator is a gradient animation (WorkingGradFrom/To
    -- = primary→secondary), not a frame sequence. We approximate with a
    -- dot-morph that cycles through the Charmtone ramp's hue range; the
    -- gradient effect itself is a WS10.3 API gap (no gradient-spec member on
    -- SpinnerSpec).
    spinner = {
        frames = { "·", "•", "●", "•", "·", " ", "·", "•", "●", "•" },
        frame_interval_ms = 120,
    },

    -- ── Thinking: crush has no whimsical verbs ────────────────────────────
    -- crush's working indicator is the gradient animation; there is no
    -- thinking-verb surface. We leave thinking unset (falls through to phi's
    -- base expressions) so the visual identity crush carries is the palette
    -- + chrome, not the verbs.
    thinking = {},

    -- ── Chrome: card container, Charmtone glyphs, `::: ` prompt ───────────
    -- crush renders tool calls as `● ToolName params` (toolHeader, tools.go:
    -- IconPending=●/IconSuccess=✓/IconError=×, NameNormal=info-coloured).
    -- Content sits on bgLeastVisible with a 2-space left pad (Tool.Body).
    -- The editor prompt is `::: ` (successMostSubtle, quickstyle.go
    -- Editor.PromptNormalFocused). User messages carry a left border
    -- (primary, NormalBorder) and assistant messages a `▌` thick left border
    -- (successMostSubtle) — phi's user_background + user_prefix approximate
    -- the wash; the border characters are a WS10.3 API gap (no border
    -- member on ChromeSpec).
    chrome = {
        anchor_format = "{verb} {args}",
        glyph_ok = "✓",
        glyph_failed = "×",
        result_prefix = "  ",
        more_lines = "… ({n} more lines)",
        container = "card",
        status_indicator = "spinner",
        status_effect = "gradient",
        status_meta = true,
        input_prompt = ":::",
        input_ruled = false,
        user_prefix = "▌ ",
        user_background = true,
        assistant_prefix = "▌ ",
        pending_indicator = "spinner_shimmer",
        pending_note = "Waiting for tool response...",
    },

    -- ── Tool renderers: crush verbs ────────────────────────────────────────
    -- Bash / View / Write / Edit / Search / Search / List over the same
    -- prelude helpers (ref/crush internal/ui/chat/{bash,file,fetch}.go).
    -- The chrome's anchor_format `{verb} {args}` renders them `Bash cmd`
    -- / `View path` matching crush's toolHeader output.
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
            verb = "View",
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
