-- packages/agent/src/agent.ts and agent-loop.ts, translated to Lua.
-- Rust remains the provider/cancellation mechanism; streamed-turn policy lives here.
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

local function shallow_copy(value)
  local result = {}
  for key, item in pairs(value) do result[key] = item end
  return result
end

local function default_convert_to_llm(messages)
  local result = {}
  for _, message in ipairs(messages) do
    if message.role == "user" or message.role == "assistant" or message.role == "toolResult" then
      result[#result + 1] = message
    end
  end
  return result
end

-- Port of streamAssistantResponse. stream_fn follows the public
-- pi.ai.stream_simple callback shape: (model, context, options, on_event).
local function stream_assistant_response(context, config, signal, emit, stream_fn)
  local messages = context.messages
  if config.transformContext then
    messages = config.transformContext(messages, signal)
  end
  local convert = config.convertToLlm or default_convert_to_llm
  local llm_context = {
    systemPrompt = context.systemPrompt,
    messages = convert(messages),
    tools = context.tools,
  }

  local api_key = config.apiKey
  if config.getApiKey then
    api_key = config.getApiKey(config.model.provider) or api_key
  end
  local stream = stream_fn or config.streamFn or pi.ai.stream_simple
  local options = shallow_copy(config)
  options.apiKey = api_key
  options.signal = signal

  local partial = nil
  local added_partial = false
  local final_message = stream(config.model, llm_context, options, function(event)
    if event.type == "start" then
      partial = event.partial
      context.messages[#context.messages + 1] = partial
      added_partial = true
      emit({ type = "message_start", message = shallow_copy(partial) })
    elseif event.type ~= "done" and event.type ~= "error" and partial then
      partial = event.partial
      context.messages[#context.messages] = partial
      emit({ type = "message_update", assistantMessageEvent = event,
             message = shallow_copy(partial) })
    end
  end)

  if added_partial then
    context.messages[#context.messages] = final_message
  else
    context.messages[#context.messages + 1] = final_message
    emit({ type = "message_start", message = shallow_copy(final_message) })
  end
  emit({ type = "message_end", message = final_message })
  return final_message
end

-- Deterministic one-turn low-level loop slice. Tool continuation is added by
-- WS4.3; this already preserves pi's prompt and assistant lifecycle ordering.
local function run_turn(prompts, context, config, emit, signal, stream_fn)
  prompts = prompts or {}
  context.messages = copy_array(context.messages)
  emit({ type = "agent_start" })
  emit({ type = "turn_start" })
  for _, prompt in ipairs(prompts) do
    context.messages[#context.messages + 1] = prompt
    emit({ type = "message_start", message = prompt })
    emit({ type = "message_end", message = prompt })
  end
  local message = stream_assistant_response(context, config, signal, emit, stream_fn)
  emit({ type = "turn_end", message = message, toolResults = {} })
  local result = copy_array(prompts)
  result[#result + 1] = message
  emit({ type = "agent_end", messages = result })
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

pi.agent = {
  new = new_agent,
  run_turn = run_turn,
  stream_assistant_response = stream_assistant_response,
}
