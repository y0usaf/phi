-- Exercises WS6.5 autocomplete through the public Lua seam.
local pi = ...
pi.register_command("tui-autocomplete-demo", {
  description = "Complete commands and paths",
  handler = function()
    local result = pi.tui.autocomplete({
      { name = "model", description = "choose a model" },
      { name = "resume", description = "resume a session" },
    }, pi.cwd(), "/mo", false)
    local item = result.items[1]
    local applied = pi.tui.apply_completion("/mo", 3, item, result.prefix)
    return { suggestions = result.items, line = applied.line, cursor = applied.cursor }
  end,
})
