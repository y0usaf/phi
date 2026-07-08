-- Built-in slash commands (PLAN.md WS3.3), shipped as embedded Lua through
-- the same phi.register_command door user extensions use (The Rule). A
-- handler returns { message } to print a • line, and/or { action } — an
-- open string key the frontend dispatches on (never a closed enum), with
-- any extra keys carried alongside as the action's arguments.

phi.register_command({
  name = "help",
  description = "list available commands",
  handler = function()
    local lines = { "commands:" }
    for _, cmd in ipairs(phi.commands()) do
      if cmd.description ~= nil and cmd.description ~= "" then
        lines[#lines + 1] = "/" .. cmd.name .. " — " .. cmd.description
      else
        lines[#lines + 1] = "/" .. cmd.name
      end
    end
    return { message = table.concat(lines, "\n") }
  end,
})

phi.register_command({
  name = "quit",
  description = "quit phi",
  handler = function()
    return { action = "quit" }
  end,
})

phi.register_command({
  name = "clear",
  description = "clear the conversation and screen",
  handler = function()
    return { action = "clear" }
  end,
})

phi.register_command({
  name = "model",
  description = "show or switch the model: /model [id]",
  handler = function(args)
    return { action = "set_model", query = args or "" }
  end,
})

phi.register_command({
  name = "tree",
  description = "browse the session tree and navigate to a point",
  handler = function()
    return { action = "open_tree" }
  end,
})

phi.register_command({
  name = "fork",
  description = "fork from a prior user message into a new session",
  handler = function()
    return { action = "fork" }
  end,
})

phi.register_command({
  name = "login",
  description = "provider login: /login anthropic|openai [device]",
  handler = function(args)
    return { action = "login", args = args or "" }
  end,
})

phi.register_command({
  name = "logout",
  description = "remove stored credentials: /logout anthropic|openai",
  handler = function(args)
    return { action = "logout", args = args or "" }
  end,
})
