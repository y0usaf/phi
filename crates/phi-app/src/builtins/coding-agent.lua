-- The default one-shot coding agent pack. Policy stays in Lua; Rust only
-- provides the provider, tools, and JSONL persistence mechanisms.
local pi = ...

pi.register_command("phi-run", { handler = function(args, ctx)
  local request = pi.json.decode(args)
  local events = {}
  local agent = pi.agent.new({
    initialState = { model = request.model, tools = pi.registered_tools(), messages = {} },
    apiKey = request.apiKey,
    on_event = function(event) events[#events + 1] = event end,
  })
  agent:subscribe(function(event)
    if event.type == "message_update" and event.assistantMessageEvent and
       event.assistantMessageEvent.type == "text_delta" then
      pi.output(event.assistantMessageEvent.delta or "")
    end
    if event.type == "message_end" and event.message and
       (event.message.role == "user" or event.message.role == "assistant" or
        event.message.role == "toolResult") and request.sessionPath then
      pi.session_append(request.sessionPath, event.message)
    end
  end)
  if request.sessionPath then
    pi.session_append(request.sessionPath, {
      role = "user", content = {{ type = "text", text = request.prompt }},
      timestamp = os.time() * 1000,
    })
  end
  agent:prompt(request.prompt)
  local state = agent:get_state()
  local last = state.messages[#state.messages]
  local text = ""
  if last and last.role == "assistant" then
    for _, part in ipairs(last.content or {}) do
      if part.type == "text" then text = text .. (part.text or "") end
    end
  end
  return { text = text, events = events, sessionPath = request.sessionPath }
end })
