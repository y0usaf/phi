-- Phi's default tool display metadata.
--
-- Declarative render specs for the prelude coding helpers (bash, read_file,
-- write_file, edit_file, grep, find, ls), registered through
-- `phi.register_tool_renderer` like any user extension would. This is where
-- the frontends' knowledge of `bash`/`read_file`/… display shapes comes from.
--
-- A user extension that registers a renderer for the same tool name wins
-- (last-write-wins) — restyling a builtin tool is supported.

-- ── bash: shell command with exit code trailer ────────────────────────

phi.register_tool_renderer("bash", {
    verb = "$",
    display_name = "Shell",
    category = "commands",
    body = { kind = "code", key = "cmd", lang = "bash", prompt = "$" },
    exit_code_trailer = true,
})

-- ── File operations ─────────────────────────────────────────────────────

phi.register_tool_renderer("read_file", {
    verb = "read",
    category = "reads",
    anchor = { { key = "path", style = "path" } },
    body = { kind = "none" },
    result_lang = { kind = "path_arg", key = "path" },
})

phi.register_tool_renderer("write_file", {
    verb = "write",
    category = "edits",
    anchor = { { key = "path", style = "path" } },
    body = { kind = "none" },
})

phi.register_tool_renderer("edit_file", {
    verb = "edit",
    category = "edits",
    anchor = { { key = "path", style = "path" } },
    body = { kind = "none" },
})

-- ── Search tools ────────────────────────────────────────────────────────

phi.register_tool_renderer("grep", {
    verb = "grep",
    category = "searches",
    anchor = {
        { key = "pattern", style = "pattern" },
        { key = "path", style = "path", prefix = "in", skip_values = { "." } },
    },
    body = { kind = "none" },
})

phi.register_tool_renderer("find", {
    verb = "find",
    category = "searches",
    anchor = {
        { key = "pattern", style = "pattern" },
        { key = "path", style = "path", prefix = "in", skip_values = { "." } },
    },
    body = { kind = "none" },
})

-- ── ls: directory listing ───────────────────────────────────────────────

phi.register_tool_renderer("ls", {
    verb = "ls",
    anchor = { { key = "path", style = "path" } },
    body = { kind = "none" },
})
