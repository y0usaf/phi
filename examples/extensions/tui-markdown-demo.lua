-- Exercises the WS6.6 markdown mechanism through the public Lua seam.
local pi = ...
pi.register_command("tui-markdown-demo", {
  description = "Render markdown cells",
  handler = function()
    return pi.tui.markdown_render("# Ready\n\n- exact\n- deterministic", 32, 1, 0)
  end,
})
