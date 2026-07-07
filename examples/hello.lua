-- hello: the smallest possible pack — one command, one event handler.
-- The examples directory is the conformance suite: every hook the host
-- exposes has at least one exerciser here (DESIGN.md API growth rule).
--
-- Install by copying into ~/.config/phi/extensions/ (loads everywhere)
-- or <project>/.phi/extensions/ (loads after you trust the project).

phi.register_command({
  name = "hello",
  description = "reply with a greeting",
  handler = function(args)
    if args ~= nil and args ~= "" then
      return { message = "hello, " .. args .. "!" }
    end
    return { message = "hello from Lua!" }
  end,
})

phi.on("session_start", function(event)
  print("hello.lua: session started in " .. (event.cwd or "?"))
end)
