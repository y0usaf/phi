-- list-commands: exercises the phi.commands() snapshot — the read side of
-- the command registry. /commands prints every registered command with the
-- extension that registered it.
--
-- Install by copying into ~/.config/phi/extensions/ (loads everywhere)
-- or <project>/.phi/extensions/ (loads after you trust the project).

phi.register_command({
  name = "commands",
  description = "list registered commands with their sources",
  handler = function()
    local lines = {}
    for _, cmd in ipairs(phi.commands()) do
      lines[#lines + 1] = "/" .. cmd.name .. "  (" .. cmd.source .. ")"
    end
    return { message = table.concat(lines, "\n") }
  end,
})
