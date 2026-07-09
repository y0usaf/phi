-- grep.ts — ripgrep-backed content search on the public extension surface.
-- Render callbacks and AbortSignal arrive with their owning WS6/WS4 mechanisms.
local GREP_DEFAULT_LIMIT = 100
local MAX_SAFE_INTEGER = 9007199254740991

local function trim_space(value)
  return (value:gsub("^%s+", ""):gsub("%s+$", ""))
end

local function grep_display_path(search_path, is_directory, file_path)
  if is_directory then
    local relative = pi.path.relative(search_path, file_path)
    if relative ~= "" and relative:sub(1, 2) ~= ".." then
      return relative:gsub("\\", "/")
    end
  end
  return pi.path.basename(file_path)
end

pi.register_tool({
  name = "grep",
  label = "grep",
  description = "Search file contents for a pattern. Returns matching lines with file paths and line numbers. Respects .gitignore. Output is truncated to 100 matches or 50KB (whichever is hit first). Long lines are truncated to 500 chars.",
  promptSnippet = "Search file contents for patterns (respects .gitignore)",
  parameters = {
    type = "object",
    properties = {
      pattern = { type = "string", description = "Search pattern (regex or literal string)" },
      path = { type = "string", description = "Directory or file to search (default: current directory)" },
      glob = { type = "string", description = "Filter files by glob pattern, e.g. '*.ts' or '**/*.spec.ts'" },
      ignoreCase = { type = "boolean", description = "Case-insensitive search (default: false)" },
      literal = { type = "boolean", description = "Treat pattern as literal string instead of regex (default: false)" },
      context = { type = "number", description = "Number of lines to show before and after each match (default: 0)" },
      limit = { type = "number", description = "Maximum number of matches to return (default: 100)" },
    },
    required = { "pattern" },
  },
  execute = function(_tool_call_id, params)
    local search_path = resolve_to_cwd((params.path ~= nil and params.path ~= "") and params.path or ".")
    local ok_stat, stat = pcall(pi.fs.stat, search_path)
    if not ok_stat then error("Path not found: " .. search_path, 0) end
    local is_directory = stat.type == "dir"
    local effective_limit = math.max(1, params.limit or GREP_DEFAULT_LIMIT)
    local context = type(params.context) == "number" and params.context > 0 and params.context or 0

    local probe = pi.exec("rg", { "--version" })
    if probe.code ~= 0 then error("ripgrep (rg) is not available and could not be downloaded", 0) end
    local args = { "--json", "--line-number", "--color=never", "--hidden" }
    if params.ignoreCase then args[#args + 1] = "--ignore-case" end
    if params.literal then args[#args + 1] = "--fixed-strings" end
    if params.glob then args[#args + 1] = "--glob"; args[#args + 1] = params.glob end
    args[#args + 1] = "--"
    args[#args + 1] = params.pattern
    args[#args + 1] = search_path
    local result = pi.exec("rg", args)
    if result.code ~= 0 and result.code ~= 1 then
      local message = trim_space(result.stderr or "")
      error(message ~= "" and message or ("ripgrep exited with code " .. result.code), 0)
    end

    local matches = {}
    for _, line in ipairs(split(result.stdout or "", "\n")) do
      if line ~= "" and #matches < effective_limit then
        local ok, event = pcall(pi.json.decode, line)
        if ok and event.type == "match" and event.data and event.data.path then
          local file_path = event.data.path.text
          local line_number = event.data.line_number
          if type(file_path) == "string" and type(line_number) == "number" then
            matches[#matches + 1] = {
              path = file_path,
              line = line_number,
              text = event.data.lines and event.data.lines.text or nil,
            }
          end
        end
      end
    end
    if #matches == 0 then return { content = { { type = "text", text = "No matches found" } } } end

    local output_lines, lines_truncated, file_cache = {}, false, {}
    local function append_line(prefix, text)
      local sanitized = (text or ""):gsub("\r", "")
      local truncated = truncate_line(sanitized)
      if truncated.wasTruncated then lines_truncated = true end
      output_lines[#output_lines + 1] = prefix .. truncated.text
    end
    for _, match in ipairs(matches) do
      local display = grep_display_path(search_path, is_directory, match.path)
      if context == 0 and match.text ~= nil then
        local text = match.text:gsub("\r\n", "\n"):gsub("\r", ""):gsub("\n$", "")
        append_line(("%s:%d: "):format(display, match.line), text)
      else
        local lines = file_cache[match.path]
        if lines == nil then
          local ok, content = pcall(pi.fs.read_file, match.path)
          if ok then
            content = content:gsub("\r\n", "\n"):gsub("\r", "\n")
            lines = split(content, "\n")
          else
            lines = false
          end
          file_cache[match.path] = lines
        end
        if lines == false then
          output_lines[#output_lines + 1] = ("%s:%d: (unable to read file)"):format(display, match.line)
        else
          local first = math.max(1, match.line - context)
          local last = math.min(#lines, match.line + context)
          for n = first, last do
            local marker = n == match.line and ":" or "-"
            append_line(("%s%s%d%s "):format(display, marker, n, marker), lines[n] or "")
          end
        end
      end
    end

    local truncation = truncate_head(table.concat(output_lines, "\n"), { maxLines = MAX_SAFE_INTEGER })
    local output, details, notices = truncation.content, {}, {}
    local limit_reached = #matches >= effective_limit
    if limit_reached then
      notices[#notices + 1] = ("%d matches limit reached. Use limit=%d for more, or refine pattern"):format(effective_limit, effective_limit * 2)
      details.matchLimitReached = effective_limit
    end
    if truncation.truncated then
      notices[#notices + 1] = format_size(DEFAULT_MAX_BYTES) .. " limit reached"
      details.truncation = truncation
    end
    if lines_truncated then
      notices[#notices + 1] = ("Some lines truncated to %d chars. Use read tool to see full lines"):format(GREP_MAX_LINE_LENGTH)
      details.linesTruncated = true
    end
    if #notices > 0 then output = output .. "\n\n[" .. table.concat(notices, ". ") .. "]" end
    return { content = { { type = "text", text = output } }, details = next(details) and details or nil }
  end,
})
