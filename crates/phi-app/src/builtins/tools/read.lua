-- read.ts — the read tool (execute half).
--
-- Deferred to their host mechanisms (carried in PLAN.md): renderCall /
-- renderResult (theme + TUI components, WS6/WS8); image auto-resize
-- (Photon worker in pi) — images pass through unresized, so the
-- 2000x2000 / size-cap behavior and the dimension note are pending; the
-- non-vision-model note (needs ctx.model); abort via signal.
local B64_CHARS = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"

local function base64_encode(data)
  local out = {}
  local n = #data
  local i = 1
  while i + 2 <= n do
    local a, b, c = data:byte(i, i + 2)
    local v = a * 0x10000 + b * 0x100 + c
    out[#out + 1] = B64_CHARS:sub((v >> 18) + 1, (v >> 18) + 1)
      .. B64_CHARS:sub(((v >> 12) & 63) + 1, ((v >> 12) & 63) + 1)
      .. B64_CHARS:sub(((v >> 6) & 63) + 1, ((v >> 6) & 63) + 1)
      .. B64_CHARS:sub((v & 63) + 1, (v & 63) + 1)
    i = i + 3
  end
  local rest = n - i + 1
  if rest == 1 then
    local a = data:byte(i)
    local v = a * 0x10000
    out[#out + 1] = B64_CHARS:sub((v >> 18) + 1, (v >> 18) + 1)
      .. B64_CHARS:sub(((v >> 12) & 63) + 1, ((v >> 12) & 63) + 1)
      .. "=="
  elseif rest == 2 then
    local a, b = data:byte(i, i + 1)
    local v = a * 0x10000 + b * 0x100
    out[#out + 1] = B64_CHARS:sub((v >> 18) + 1, (v >> 18) + 1)
      .. B64_CHARS:sub(((v >> 12) & 63) + 1, ((v >> 12) & 63) + 1)
      .. B64_CHARS:sub(((v >> 6) & 63) + 1, ((v >> 6) & 63) + 1)
      .. "="
  end
  return table.concat(out)
end

pi.register_tool({
  name = "read",
  label = "read",
  description = (
    "Read the contents of a file. Supports text files and images (jpg, png, gif, webp)."
    .. " Images are sent as attachments. For text files, output is truncated to %d lines"
    .. " or %dKB (whichever is hit first). Use offset/limit for large files. When you"
    .. " need the full file, continue with offset until complete."
  ):format(DEFAULT_MAX_LINES, DEFAULT_MAX_BYTES // 1024),
  promptSnippet = "Read file contents",
  promptGuidelines = { "Use read to examine files instead of cat or sed." },
  parameters = {
    type = "object",
    properties = {
      path = { type = "string", description = "Path to the file to read (relative or absolute)" },
      offset = { type = "number", description = "Line number to start reading from (1-indexed)" },
      limit = { type = "number", description = "Maximum number of lines to read" },
    },
    required = { "path" },
  },
  execute = function(_tool_call_id, params)
    local path, offset, limit = params.path, params.offset, params.limit
    local absolute_path = resolve_read_path(path)
    -- Spec ops.access(R_OK): Node-shaped error so callers can match ENOENT.
    if not pi.fs.exists(absolute_path) then
      error(("ENOENT: no such file or directory, access '%s'"):format(absolute_path), 0)
    end
    local buffer = pi.fs.read_bytes(absolute_path)
    local mime_type = detect_supported_image_mime_type(buffer:sub(1, IMAGE_TYPE_SNIFF_BYTES))

    if mime_type then
      -- Spec resizes through Photon before returning; the resize mechanism
      -- is carried — original bytes pass through, no dimension note.
      return {
        content = {
          { type = "text", text = ("Read image file [%s]"):format(mime_type) },
          { type = "image", data = base64_encode(buffer), mimeType = mime_type },
        },
      }
    end

    local text_content = utf8_lossy(buffer)
    local all_lines = split(text_content, "\n")
    local total_file_lines = #all_lines
    -- Convert from 1-indexed input to 0-indexed access (spec keeps the
    -- 0-indexed startLine variable; array reads below convert back).
    local start_line = offset and math.max(0, offset - 1) or 0
    local start_line_display = start_line + 1
    if start_line >= total_file_lines then
      error(("Offset %s is beyond end of file (%d lines total)"):format(fmt_num(offset), total_file_lines), 0)
    end

    local selected_content
    local user_limited_lines
    if limit ~= nil then
      local end_line = math.min(start_line + limit, total_file_lines)
      selected_content = table.concat(all_lines, "\n", start_line + 1, end_line)
      user_limited_lines = end_line - start_line
    else
      selected_content = table.concat(all_lines, "\n", start_line + 1, total_file_lines)
    end

    local truncation = truncate_head(selected_content)
    local output_text
    local details
    if truncation.firstLineExceedsLimit then
      -- First line alone exceeds the byte limit: point at a bash fallback.
      local first_line_size = format_size(#all_lines[start_line + 1])
      output_text = ("[Line %d is %s, exceeds %s limit. Use bash: sed -n '%dp' %s | head -c %d]"):format(
        start_line_display,
        first_line_size,
        format_size(DEFAULT_MAX_BYTES),
        start_line_display,
        path,
        DEFAULT_MAX_BYTES
      )
      details = { truncation = truncation }
    elseif truncation.truncated then
      -- Truncation occurred: build an actionable continuation notice.
      local end_line_display = start_line_display + truncation.outputLines - 1
      local next_offset = end_line_display + 1
      output_text = truncation.content
      if truncation.truncatedBy == "lines" then
        output_text = output_text
          .. ("\n\n[Showing lines %d-%d of %d. Use offset=%d to continue.]"):format(
            start_line_display,
            end_line_display,
            total_file_lines,
            next_offset
          )
      else
        output_text = output_text
          .. ("\n\n[Showing lines %d-%d of %d (%s limit). Use offset=%d to continue.]"):format(
            start_line_display,
            end_line_display,
            total_file_lines,
            format_size(DEFAULT_MAX_BYTES),
            next_offset
          )
      end
      details = { truncation = truncation }
    elseif user_limited_lines ~= nil and start_line + user_limited_lines < total_file_lines then
      -- User-specified limit stopped early but the file has more content.
      local remaining = total_file_lines - (start_line + user_limited_lines)
      local next_offset = start_line + user_limited_lines + 1
      output_text = ("%s\n\n[%d more lines in file. Use offset=%d to continue.]"):format(
        truncation.content,
        remaining,
        next_offset
      )
    else
      output_text = truncation.content
    end

    return { content = { { type = "text", text = output_text } }, details = details }
  end,
})
