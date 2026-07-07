-- tool: register a custom tool the model can call natively, through the
-- same registry the shipped read/bash/edit/write tools use.
--
-- Install by copying into ~/.config/phi/extensions/ (loads everywhere)
-- or <project>/.phi/extensions/ (loads after you trust the project).

phi.register_tool({
  name = "word_count",
  description = "Count the words in a string of text.",
  parameters = {
    type = "object",
    properties = {
      text = { type = "string", description = "Text to count words in" },
    },
    required = { "text" },
  },
  handler = function(args)
    if type(args.text) ~= "string" then
      error("word_count: 'text' must be a string")
    end
    local n = 0
    for _ in string.gmatch(args.text, "%S+") do
      n = n + 1
    end
    return n .. " words"
  end,
})
