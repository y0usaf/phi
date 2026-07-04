-- Phi's default mux colon commands.
--
-- These are `:`-prefixed commands in the mux shell, tagged
-- `context = "mux"` so they're invisible in session command listings but
-- resolve (including via alias) in the mux context — the same
-- `CommandSpec` vocabulary the Rust builtins use.

-- ── :open — parse a (possibly quoted) path and expand ~ ───────────────

local function trim(s)
    return (s:match("^%s*(.-)%s*$"))
end

local function home_dir()
    return os.getenv("HOME") or "~"
end

local function expand_home(path)
    if path == "~" then return home_dir() end
    local rest = path:match("^~/(.+)$")
    if rest then return home_dir() .. "/" .. rest end
    return path
end

local function parse_path(input)
    input = trim(input)
    if input == "" then
        error("usage: :open <dir>")
    end
    local first = input:sub(1, 1)
    if first ~= "'" and first ~= '"' then
        return input
    end
    if #input == 1 or input:sub(-1) ~= first then
        error("open: unterminated quoted path")
    end
    return input:sub(2, -2)
end

-- ── Helper to register a mux command with a single fixed action ────────

local function mux_action(name, aliases, args_hint, description, action)
    phi.register_command(name, {
        description = description,
        args_hint = args_hint,
        context = "mux",
        aliases = aliases,
        handler = function()
            return { actions = { action } }
        end,
    })
end

local function host_action(name, aliases, description, action_name)
    mux_action(name, aliases, "", description,
        { type = "RunHostAction", data = { name = action_name } })
end

local function overlay_action(name, aliases, description, overlay)
    mux_action(name, aliases, "", description,
        { type = "OpenOverlay", data = { name = overlay } })
end

-- ── Register all mux commands ──────────────────────────────────────────

phi.register_command("open", {
    description = "open a directory as a project",
    args_hint = "<dir>",
    context = "mux",
    aliases = { "o" },
    handler = function(args)
        local path = parse_path(args)
        return { actions = { { type = "OpenProject", data = expand_home(path) } } }
    end,
})

overlay_action("archive", { "archives" }, "browse archived sessions", "phi:archive")
host_action("cleanup", { "clean" }, "remove old archived sessions", "cleanup")
host_action("refresh", {}, "refresh the current session", "refresh")
host_action("reload", {}, "refresh all sessions", "reload")
overlay_action("usage", {}, "show usage totals", "phi:usage")
mux_action("quit", { "q" }, "", "quit the mux", { type = "Quit" })
overlay_action("help", { "h" }, "show commands and keys", "phi:help")
host_action("up", { "prevsession" }, "previous session", "session-prev")
host_action("down", { "nextsession" }, "next session", "session-next")
host_action("left", { "prevproject" }, "previous project", "project-prev")
host_action("right", { "nextproject" }, "next project", "project-next")
