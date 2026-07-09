-- bash.ts — exact pi v0.79 execute behavior within phi's current surface.
-- Render callbacks, commandPrefix/shellPath/spawnHook factory options, and
-- AbortSignal arrive with their owning frontend/context mechanisms.
pi.register_tool({
  name = "bash",
  label = "bash",
  description = "Execute a bash command in the current working directory. Returns stdout and stderr. Output is truncated to last 2000 lines or 50KB (whichever is hit first). If truncated, full output is saved to a temp file. Optionally provide a timeout in seconds.",
  promptSnippet = "Execute bash commands (ls, grep, find, etc.)",
  parameters = {
    type = "object",
    properties = {
      command = { type = "string", description = "Bash command to execute" },
      timeout = { type = "number", description = "Timeout in seconds (optional, no default timeout)" },
    },
    required = { "command" },
  },
  execute = function(tool_call_id, params, _signal, on_update)
    if not pi.fs.exists(cwd) then
      error("Working directory does not exist: " .. cwd .. "\nCannot execute bash commands.")
    end
    local safe = tostring(tool_call_id):gsub("[^%w_.-]", "-")
    local full_path = pi.fs.create_temp_file("phi-bash-" .. safe .. "-", "")
    local output = new_output_accumulator(full_path)
    local last_update_ms, dirty = -math.huge, false
    local function update(force)
      if not on_update or not dirty then return end
      local now = pi.monotonic_ms()
      if not force and now - last_update_ms < 100 then return end
      local snapshot = output.snapshot()
      on_update({
        content = { { type = "text", text = snapshot.content or "" } },
        details = snapshot.truncation.truncated and {
          truncation = snapshot.truncation,
          fullOutputPath = snapshot.fullOutputPath,
        } or nil,
      })
      dirty, last_update_ms = false, now
    end
    if on_update then on_update({ content = {} }) end
    local shell, args = shell_config()
    args[#args + 1] = params.command
    local timeout = type(params.timeout) == "number" and params.timeout > 0
      and params.timeout * 1000 or nil
    local result = pi.exec(shell, args, {
      cwd = cwd,
      timeout = timeout,
      onData = function(data)
        pi.fs.append_file(full_path, data)
        output.append(data)
        dirty = true
        update(false)
      end,
    })
    output.finish()
    update(true)
    local snapshot = output.snapshot()
    local truncation = snapshot.truncation
    if not truncation.truncated then pi.fs.remove_file(full_path) end
    local text = snapshot.content ~= "" and snapshot.content or "(no output)"
    local details
    if truncation.truncated then
      details = { truncation = truncation, fullOutputPath = snapshot.fullOutputPath }
      local first = truncation.totalLines - truncation.outputLines + 1
      local last = truncation.totalLines
      if truncation.lastLinePartial then
        text = text .. "\n\n[Showing last " .. format_size(truncation.outputBytes) .. " of line " .. last
          .. " (line is " .. format_size(output.last_line_bytes()) .. "). Full output: " .. snapshot.fullOutputPath .. "]"
      elseif truncation.truncatedBy == "lines" then
        text = text .. "\n\n[Showing lines " .. first .. "-" .. last .. " of " .. truncation.totalLines
          .. ". Full output: " .. snapshot.fullOutputPath .. "]"
      else
        text = text .. "\n\n[Showing lines " .. first .. "-" .. last .. " of " .. truncation.totalLines
          .. " (" .. format_size(DEFAULT_MAX_BYTES) .. " limit). Full output: " .. snapshot.fullOutputPath .. "]"
      end
    end
    local function fail(status)
      local prefix = snapshot.content ~= "" and (text .. "\n\n") or ""
      error(prefix .. status)
    end
    if result.killed then fail("Command timed out after " .. fmt_num(params.timeout) .. " seconds") end
    if result.code ~= 0 then fail("Command exited with code " .. result.code) end
    return { content = { { type = "text", text = text } }, details = details }
  end,
})
