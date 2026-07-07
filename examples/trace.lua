-- trace: subscribe to every agent lifecycle event the loop mirrors onto
-- the Lua bus, and append one line per event to /tmp/phi-trace.log.
-- The exerciser for turn_start / turn_end / agent_end / session_start.
--
-- Install by copying into ~/.config/phi/extensions/ (loads everywhere)
-- or <project>/.phi/extensions/ (loads after you trust the project).

local function log(line)
  local f = io.open("/tmp/phi-trace.log", "a")
  if f then
    f:write(os.date("%H:%M:%S "), line, "\n")
    f:close()
  end
end

phi.on("session_start", function(event)
  log("session_start cwd=" .. (event.cwd or "?"))
end)

phi.on("turn_start", function(event)
  log("turn_start index=" .. tostring(event.index))
end)

phi.on("turn_end", function(event)
  log("turn_end index=" .. tostring(event.index) .. " tool_calls=" .. tostring(event.tool_calls))
end)

phi.on("agent_end", function(event)
  log("agent_end turns=" .. tostring(event.turns))
end)
