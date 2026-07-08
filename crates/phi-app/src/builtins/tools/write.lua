-- write.ts — the write tool (execute half). renderCall/renderResult
-- (syntax-highlight cache, theme) land with the WS6 TUI bindings; abort
-- via signal lands with the cancellation mechanism.
--
-- Divergence noted: the spec's success message counts JS UTF-16 units
-- ("bytes" in the text); this counts actual bytes — equal for ASCII.
pi.register_tool({
  name = "write",
  label = "write",
  description = "Write content to a file. Creates the file if it doesn't exist, overwrites if it does."
    .. " Automatically creates parent directories.",
  promptSnippet = "Create or overwrite files",
  promptGuidelines = { "Use write only for new files or complete rewrites." },
  parameters = {
    type = "object",
    properties = {
      path = { type = "string", description = "Path to the file to write (relative or absolute)" },
      content = { type = "string", description = "Content to write to the file" },
    },
    required = { "path", "content" },
  },
  execute = function(_tool_call_id, params)
    local path, content = params.path, params.content
    local absolute_path = resolve_to_cwd(path)
    local dir = pi.path.dirname(absolute_path)
    return with_file_mutation_queue(absolute_path, function()
      -- Create parent directories if needed, then write.
      pi.fs.mkdir(dir)
      pi.fs.write_file(absolute_path, content)
      return {
        content = { { type = "text", text = ("Successfully wrote %d bytes to %s"):format(#content, path) } },
      }
    end)
  end,
})
