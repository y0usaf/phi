-- find.ts — fd-backed glob search on the public extension surface.
-- Render callbacks and AbortSignal arrive with their owning WS6/WS4 mechanisms.
local FIND_DEFAULT_LIMIT = 1000
local MAX_SAFE_INTEGER = 9007199254740991

local function find_trim(value)
  return (value:gsub("^%s+", ""):gsub("%s+$", ""))
end

pi.register_tool({
  name = "find",
  label = "find",
  description = "Search for files by glob pattern. Returns matching file paths relative to the search directory. Respects .gitignore. Output is truncated to 1000 results or 50KB (whichever is hit first).",
  promptSnippet = "Find files by glob pattern (respects .gitignore)",
  parameters = {
    type = "object",
    properties = {
      pattern = { type = "string", description = "Glob pattern to match files, e.g. '*.ts', '**/*.json', or 'src/**/*.spec.ts'" },
      path = { type = "string", description = "Directory to search in (default: current directory)" },
      limit = { type = "number", description = "Maximum number of results (default: 1000)" },
    },
    required = { "pattern" },
  },
  execute = function(_tool_call_id, params)
    local search_path = resolve_to_cwd((params.path ~= nil and params.path ~= "") and params.path or ".")
    if not pi.fs.exists(search_path) then error("Path not found: " .. search_path, 0) end
    local effective_limit = params.limit or FIND_DEFAULT_LIMIT

    local command = "fd"
    local probe = pi.exec(command, { "--version" })
    if probe.code ~= 0 then
      command = "fdfind"
      probe = pi.exec(command, { "--version" })
    end
    if probe.code ~= 0 then error("fd is not available and could not be downloaded", 0) end

    local args = { "--glob", "--color=never", "--hidden", "--no-require-git", "--max-results", tostring(effective_limit) }
    local pattern = params.pattern
    if pattern:find("/", 1, true) then
      args[#args + 1] = "--full-path"
      if pattern:sub(1, 1) ~= "/" and pattern:sub(1, 3) ~= "**/" and pattern ~= "**" then
        pattern = "**/" .. pattern
      end
    end
    args[#args + 1] = "--"
    args[#args + 1] = pattern
    args[#args + 1] = search_path
    local result = pi.exec(command, args)
    local stdout = result.stdout or ""
    if result.code ~= 0 and stdout == "" then
      local message = find_trim(result.stderr or "")
      error(message ~= "" and message or ("fd exited with code " .. result.code), 0)
    end
    if stdout == "" then
      return { content = { { type = "text", text = "No files found matching pattern" } } }
    end

    local paths = {}
    for _, raw in ipairs(split(stdout, "\n")) do
      local line = find_trim(raw:gsub("\r$", ""))
      if line ~= "" then
        local trailing = line:sub(-1) == "/" or line:sub(-1) == "\\"
        local relative
        if line:sub(1, #search_path) == search_path then
          relative = line:sub(#search_path + 1):gsub("^[/\\]", "")
        else
          relative = pi.path.relative(search_path, line)
        end
        relative = relative:gsub("\\", "/")
        if trailing and relative:sub(-1) ~= "/" then relative = relative .. "/" end
        paths[#paths + 1] = relative
      end
    end
    if #paths == 0 then
      return { content = { { type = "text", text = "No files found matching pattern" } } }
    end

    local truncation = truncate_head(table.concat(paths, "\n"), { maxLines = MAX_SAFE_INTEGER })
    local output, details, notices = truncation.content, {}, {}
    if #paths >= effective_limit then
      notices[#notices + 1] = ("%d results limit reached. Use limit=%d for more, or refine pattern"):format(effective_limit, effective_limit * 2)
      details.resultLimitReached = effective_limit
    end
    if truncation.truncated then
      notices[#notices + 1] = format_size(DEFAULT_MAX_BYTES) .. " limit reached"
      details.truncation = truncation
    end
    if #notices > 0 then output = output .. "\n\n[" .. table.concat(notices, ". ") .. "]" end
    return { content = { { type = "text", text = output } }, details = next(details) and details or nil }
  end,
})
