-- Phi's coding tools as native tool definitions (hybrid surface, WS9.3b).
--
-- The prelude's coding helpers (bash, read_file, write_file, edit_file, grep,
-- find, ls) registered with `expose = "both"` + `dispatch = "python"`: the
-- provider sees native tool definitions, and every native call maps onto the
-- *same* prelude functions in the same REPL env (`__phi_native_call__`), so the
-- hashline cache, read-before-edit enforcement, and output spill are shared
-- regardless of surface — one implementation, two exposures.
--
-- Enabled with `coding_tools_native = true` in config.lua.
--
-- A user extension that registers a tool with the same name and
-- `expose = "both"` can override these (last-write-wins for tool specs).
--
-- Params mirror the Python signatures (defaults are Python literals); the
-- explicit schemas add the per-field descriptions the derived schema can't carry.

local function param(name, ty, default)
    return { name = name, type = ty, default = default }
end

local function spec(name, description, params, input_schema)
    phi.register_tool(name, {
        description = description,
        params = params,
        expose = "both",
        dispatch = "python",
        input_schema = input_schema,
    })
end

-- ── bash: shell command runner ───────────────────────────────────────────

spec("bash",
    "Run a shell command in the working directory; returns combined stdout/stderr "
    .. "plus a note on non-zero exit. Output over 50KB spills to a tmp file with a "
    .. "[full output: path] annotation. For long-running work (builds, test suites, "
    .. "servers) prefer the repl tool with a background job: "
    .. "jid = bg(\"out = bash('cargo build', timeout=3600)\").",
    {
        param("cmd", "str", nil),
        param("timeout", "int", "120"),
    },
    {
        type = "object",
        properties = {
            cmd = { type = "string", description = "Shell command to run." },
            timeout = { type = "integer", description = "Seconds before the command is killed (default 120)." },
        },
        required = { "cmd" },
    })

-- ── File operations ──────────────────────────────────────────────────────

spec("read_file",
    "Read a file as `[path#TAG]` + `LINE:TEXT` hashline rows (default) or plain text "
    .. "(raw=true). Repeated reads of an unchanged file return a one-line stub instead of "
    .. "re-emitting content — pass fresh=true to force a full re-read.",
    {
        param("path", "str", nil),
        param("offset", "int", "0"),
        param("limit", "int", "2000"),
        param("raw", "bool", "False"),
        param("fresh", "bool", "False"),
    },
    {
        type = "object",
        properties = {
            path = { type = "string", description = "File to read." },
            offset = { type = "integer", description = "Line to start from (default 0)." },
            limit = { type = "integer", description = "Max lines to read (default 2000)." },
            raw = { type = "boolean", description = "Plain text without line numbers or hashline tags (default false)." },
            fresh = { type = "boolean", description = "Force a full re-read of an unchanged file (default false)." },
        },
        required = { "path" },
    })

spec("write_file",
    "Write a file, creating parent directories. Refuses to overwrite an existing file "
    .. "this session hasn't read — call read_file(path) first, then retry.",
    {
        param("path", "str", nil),
        param("content", "str", nil),
    },
    {
        type = "object",
        properties = {
            path = { type = "string", description = "File to write." },
            content = { type = "string", description = "Full file content." },
        },
        required = { "path", "content" },
    })

spec("edit_file",
    "Edit a file: exact string replacement (path + old + new) OR hashline patch "
    .. "application (patch only — `[path#TAG]` + `SWAP`/`DEL`/`INS.*` ops with "
    .. "snapshot-tag checks and stale-tag recovery). Refuses to edit a file this session "
    .. "hasn't read or that changed on disk since the last read — re-read with "
    .. "read_file(path, fresh=true), rebase the change, then retry.",
    {
        param("path", "str", "None"),
        param("old", "str", "None"),
        param("new", "str", "None"),
        param("replace_all", "bool", "False"),
        param("patch", "str", "None"),
    },
    {
        type = "object",
        properties = {
            path = { type = "string", description = "File to edit (string-replacement mode)." },
            old = { type = "string", description = "Exact text to replace; must be unique unless replace_all." },
            new = { type = "string", description = "Replacement text." },
            replace_all = { type = "boolean", description = "Replace every occurrence of `old` (default false)." },
            patch = { type = "string", description = "Hashline patch to apply instead of old/new (carries its own paths and tags)." },
        },
        required = {},
    })

-- ── Search tools ─────────────────────────────────────────────────────────

spec("grep",
    "Regex search across files (skips .git, node_modules, target, etc.). Returns "
    .. "`[path#TAG]` + `LINE:TEXT` hashline rows by default; raw=true for plain "
    .. "`path:line: text` output.",
    {
        param("pattern", "str", nil),
        param("path", "str", "\".\""),
        param("glob", "str", "None"),
        param("max_results", "int", "200"),
        param("raw", "bool", "False"),
    },
    {
        type = "object",
        properties = {
            pattern = { type = "string", description = "Regex to search for." },
            path = { type = "string", description = "Directory or file to search (default \".\")." },
            glob = { type = "string", description = "Filename glob filter, e.g. \"*.rs\"." },
            max_results = { type = "integer", description = "Max matching lines (default 200)." },
            raw = { type = "boolean", description = "Plain `path:line: text` output (default false)." },
        },
        required = { "pattern" },
    })

spec("find",
    "Find files by glob pattern on filename.",
    {
        param("pattern", "str", nil),
        param("path", "str", "\".\""),
        param("max_results", "int", "200"),
    },
    {
        type = "object",
        properties = {
            pattern = { type = "string", description = "Filename glob, e.g. \"*.lua\"." },
            path = { type = "string", description = "Directory to search (default \".\")." },
            max_results = { type = "integer", description = "Max results (default 200)." },
        },
        required = { "pattern" },
    })

-- ── ls: directory listing ────────────────────────────────────────────────

spec("ls",
    "List a directory (dirs end with /).",
    {
        param("path", "str", "\".\""),
    },
    {
        type = "object",
        properties = {
            path = { type = "string", description = "Directory to list (default \".\")." },
        },
        required = {},
    })
