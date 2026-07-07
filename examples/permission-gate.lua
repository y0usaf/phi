-- permission-gate: veto tool calls before they run, via the blockable
-- `tool_call` event. Any handler returning { block = true, reason } stops
-- the call; the model receives an error tool-result instead.
--
-- Install by copying into ~/.config/phi/extensions/ (loads everywhere)
-- or <project>/.phi/extensions/ (loads after you trust the project).

phi.on("tool_call", function(event)
  if event.tool_name ~= "bash" then
    return
  end
  local command = (event.input and event.input.command) or ""
  -- Refuse the classics. This is an example policy, not a sandbox.
  for _, pattern in ipairs({ "rm %-rf /", "git push %-%-force", "sudo " }) do
    if string.find(command, pattern) then
      return { block = true, reason = "permission-gate.lua refused: " .. command }
    end
  end
end)
