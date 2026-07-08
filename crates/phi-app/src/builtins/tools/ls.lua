-- ls.ts — the ls tool (execute half). renderCall/renderResult land with
-- the WS6 TUI bindings; abort via signal lands with the cancellation
-- mechanism.
--
-- Divergences noted: the spec sorts with localeCompare on lowercased
-- names (ICU collation, stable); this sorts by lowercased byte order —
-- identical for ASCII names.
local LS_DEFAULT_LIMIT = 500
local MAX_SAFE_INTEGER = 9007199254740991 -- JS Number.MAX_SAFE_INTEGER

pi.register_tool({
  name = "ls",
  label = "ls",
  description = (
    "List directory contents. Returns entries sorted alphabetically, with '/' suffix for"
    .. " directories. Includes dotfiles. Output is truncated to %d entries or %dKB"
    .. " (whichever is hit first)."
  ):format(LS_DEFAULT_LIMIT, DEFAULT_MAX_BYTES // 1024),
  promptSnippet = "List directory contents",
  parameters = {
    type = "object",
    properties = {
      path = { type = "string", description = "Directory to list (default: current directory)" },
      limit = { type = "number", description = "Maximum number of entries to return (default: 500)" },
    },
  },
  execute = function(_tool_call_id, params)
    local path, limit = params.path, params.limit
    local dir_path = resolve_to_cwd((path ~= nil and path ~= "") and path or ".")
    local effective_limit = limit or LS_DEFAULT_LIMIT

    if not pi.fs.exists(dir_path) then
      error(("Path not found: %s"):format(dir_path), 0)
    end
    local st = pi.fs.stat(dir_path)
    if st.type ~= "dir" then
      error(("Not a directory: %s"):format(dir_path), 0)
    end
    local ok, entries = pcall(pi.fs.read_dir, dir_path)
    if not ok then
      error(("Cannot read directory: %s"):format(tostring(entries)), 0)
    end

    -- Sort alphabetically, case-insensitive.
    table.sort(entries, function(a, b)
      return a:lower() < b:lower()
    end)

    -- Format entries with directory indicators.
    local results = {}
    local entry_limit_reached = false
    for _, entry in ipairs(entries) do
      if #results >= effective_limit then
        entry_limit_reached = true
        break
      end
      local full_path = pi.path.join(dir_path, entry)
      local ok_stat, entry_stat = pcall(pi.fs.stat, full_path)
      -- Skip entries we cannot stat.
      if ok_stat then
        results[#results + 1] = entry .. (entry_stat.type == "dir" and "/" or "")
      end
    end

    if #results == 0 then
      return { content = { { type = "text", text = "(empty directory)" } } }
    end

    local raw_output = table.concat(results, "\n")
    -- Byte truncation only: entry count is already capped.
    local truncation = truncate_head(raw_output, { maxLines = MAX_SAFE_INTEGER })
    local output = truncation.content
    local details = {}
    -- Actionable notices for truncation and entry limits.
    local notices = {}
    if entry_limit_reached then
      notices[#notices + 1] = ("%d entries limit reached. Use limit=%d for more"):format(
        effective_limit,
        effective_limit * 2
      )
      details.entryLimitReached = effective_limit
    end
    if truncation.truncated then
      notices[#notices + 1] = ("%s limit reached"):format(format_size(DEFAULT_MAX_BYTES))
      details.truncation = truncation
    end
    if #notices > 0 then
      output = output .. ("\n\n[%s]"):format(table.concat(notices, ". "))
    end

    return {
      content = { { type = "text", text = output } },
      details = next(details) ~= nil and details or nil,
    }
  end,
})
