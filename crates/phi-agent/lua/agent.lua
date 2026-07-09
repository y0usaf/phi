-- packages/agent/src/agent.ts, translated to the public Lua surface.
-- WS4.1 owns state and mutators; streamed turns arrive in the next slice.
local pi = ...

local UNKNOWN_MODEL = {
  id = "unknown", name = "unknown", api = "unknown", provider = "unknown",
  baseUrl = "", reasoning = false, input = {},
  cost = { input = 0, output = 0, cacheRead = 0, cacheWrite = 0 },
  contextWindow = 0, maxTokens = 0,
}

local function copy_array(values)
  local result = {}
  for i, value in ipairs(values or {}) do result[i] = value end
  return result
end

local function new_agent(options)
  options = options or {}
  local initial = options.initialState or {}
  local listeners = {}
  local tools = copy_array(initial.tools)
  local messages = copy_array(initial.messages)
  local steering = {}
  local follow_up = {}
  local state = {
    systemPrompt = initial.systemPrompt or "",
    model = initial.model or UNKNOWN_MODEL,
    thinkingLevel = initial.thinkingLevel or "off",
    isStreaming = false,
    pendingToolCalls = {},
  }

  local agent = {}
  function agent:get_state()
    state.tools = tools
    state.messages = messages
    return state
  end
  function agent:set_system_prompt(value) state.systemPrompt = value end
  function agent:set_model(value) state.model = value end
  function agent:set_thinking_level(value) state.thinkingLevel = value end
  function agent:set_tools(value) tools = copy_array(value) end
  function agent:set_messages(value) messages = copy_array(value) end
  function agent:append_message(value) messages[#messages + 1] = value end
  function agent:clear_messages() messages = {} end
  function agent:subscribe(listener)
    listeners[#listeners + 1] = listener
    local active = true
    return function()
      if not active then return end
      active = false
      for i, candidate in ipairs(listeners) do
        if candidate == listener then table.remove(listeners, i); break end
      end
    end
  end
  function agent:steer(message) steering[#steering + 1] = message end
  function agent:follow_up(message) follow_up[#follow_up + 1] = message end
  function agent:has_queued_messages() return #steering > 0 or #follow_up > 0 end
  function agent:clear_steering_queue() steering = {} end
  function agent:clear_follow_up_queue() follow_up = {} end
  function agent:clear_all_queues() steering = {}; follow_up = {} end
  function agent:abort() if state.signal then state.signal:abort() end end
  function agent:reset()
    messages = {}
    state.isStreaming = false
    state.streamingMessage = nil
    state.pendingToolCalls = {}
    state.errorMessage = nil
    steering = {}
    follow_up = {}
  end
  return agent
end

pi.agent = { new = new_agent }
