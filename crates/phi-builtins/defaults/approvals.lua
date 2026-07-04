-- Phi Approvals: tool approval gate.
--
-- Evaluates every intercepted tool call against an ordered ruleset and
-- blocks, allows, or asks the user. Fail direction: asks fail closed.
--
-- ## Runtime globals (injected before this source runs)
--
--   _phi_approvals_cwd     = "/path/to/project"
--   _phi_approvals_rules   = { { tool = "bash", pattern = "git push*", action = "deny" }, ... }
--   _phi_approvals_store_path = "/path/to/approvals.json"  (optional; for tests)
--
-- ## Ruleset order (later entries win)
--
--   1. Builtin defaults: ask on repl/bash/write_file/edit_file
--   2. Config rules (from config.lua's `approvals.rules`)
--   3. Persisted "always allow" answers (from approvals.json)
--   4. Session-scoped answers (in-memory only)
--
-- A call no rule matches proceeds — the defaults cover the mutating surfaces.

--------------------------------------------------------------------------------
-- Constants
--------------------------------------------------------------------------------

local ASK_WAIT = 300        -- seconds; how long phi.ui.ask waits
local HANDLER_TIMEOUT = 310 -- seconds; watchdog above ASK_WAIT
local SUBJECT_PREVIEW_CHARS = 2000

--------------------------------------------------------------------------------
-- Minimal JSON encode/decode (for persistence)
--------------------------------------------------------------------------------

local function json_escape(s)
  return (s:gsub('\\', '\\\\'):gsub('"', '\\"'):gsub('\n', '\\n'):gsub('\r', '\\r'):gsub('\t', '\\t'))
end

--- Encode a simple Lua table (string keys, array-of-tables values) to JSON.
local function json_encode(data)
  if data == nil then return "null" end
  if type(data) == "string" then return '"' .. json_escape(data) .. '"' end
  if type(data) == "number" or type(data) == "boolean" then return tostring(data) end
  if type(data) ~= "table" then return "null" end

  -- Detect array vs object: if keys are 1..n, it's an array.
  local is_array = true
  local n = 0
  for k, _ in pairs(data) do
    n = n + 1
    if type(k) ~= "number" then is_array = false; break end
  end
  if is_array and n > 0 then
    local parts = {}
    for i = 1, n do
      parts[i] = json_encode(data[i])
    end
    return "[" .. table.concat(parts, ",") .. "]"
  elseif n == 0 then
    -- Empty: could be array or object. We use {}.
    return "{}"
  end

  local parts = {}
  for k, v in pairs(data) do
    parts[#parts + 1] = '"' .. json_escape(tostring(k)) .. '":' .. json_encode(v)
  end
  return "{" .. table.concat(parts, ",") .. "}"
end

--- Minimal JSON string decoder for our persistence format:
--- { "path1": [{tool,pattern,action},...], "path2": [...] }
--- Uses pattern matching (not a full JSON parser, but sufficient for our needs).
local function json_decode(s)
  if not s or s:match("^%s*$") then return {} end
  -- For our simple structure, we parse with a recursive descent approach.
  local pos = 1
  local function skip_ws()
    while pos <= #s and s:sub(pos, pos):match("%s") do pos = pos + 1 end
  end
  local function parse_value()
    skip_ws()
    local c = s:sub(pos, pos)
    if c == '"' then
      pos = pos + 1
      local result = {}
      while pos <= #s do
        local ch = s:sub(pos, pos)
        if ch == '\\' then
          local next_ch = s:sub(pos + 1, pos + 1)
          if next_ch == 'n' then result[#result + 1] = '\n'
          elseif next_ch == 'r' then result[#result + 1] = '\r'
          elseif next_ch == 't' then result[#result + 1] = '\t'
          elseif next_ch == '"' then result[#result + 1] = '"'
          elseif next_ch == '\\' then result[#result + 1] = '\\'
          else result[#result + 1] = next_ch
          end
          pos = pos + 2
        elseif ch == '"' then
          pos = pos + 1
          return table.concat(result)
        else
          result[#result + 1] = ch
          pos = pos + 1
        end
      end
      return ""
    elseif c == '{' then
      pos = pos + 1
      local obj = {}
      skip_ws()
      if s:sub(pos, pos) == '}' then pos = pos + 1; return obj end
      while true do
        skip_ws()
        local key = parse_value() -- should be a string
        skip_ws()
        if s:sub(pos, pos) == ':' then pos = pos + 1 end
        local val = parse_value()
        obj[key] = val
        skip_ws()
        if s:sub(pos, pos) == ',' then pos = pos + 1
        elseif s:sub(pos, pos) == '}' then pos = pos + 1; break
        else break end
      end
      return obj
    elseif c == '[' then
      pos = pos + 1
      local arr = {}
      skip_ws()
      if s:sub(pos, pos) == ']' then pos = pos + 1; return arr end
      while true do
        arr[#arr + 1] = parse_value()
        skip_ws()
        if s:sub(pos, pos) == ',' then pos = pos + 1
        elseif s:sub(pos, pos) == ']' then pos = pos + 1; break
        else break end
      end
      return arr
    elseif c == 't' then
      pos = pos + 4
      return true
    elseif c == 'f' then
      pos = pos + 5
      return false
    elseif c == 'n' then
      pos = pos + 4
      return nil
    elseif c:match("[%d%-]") then
      local start = pos
      while pos <= #s and s:sub(pos, pos):match("[%d%.%-eE+]") do pos = pos + 1 end
      return tonumber(s:sub(start, pos - 1))
    else
      pos = pos + 1
      return nil
    end
  end
  return parse_value()
end

--------------------------------------------------------------------------------
-- Wildcard matching (* = any run, ? = one char)
--------------------------------------------------------------------------------

local function wildcard_match(pattern, text)
  local p, t = pattern, text
  local pi, ti = 1, 1
  local star, mark
  while ti <= #t do
    if pi <= #p and (p:sub(pi, pi) == "?" or p:sub(pi, pi) == t:sub(ti, ti)) then
      pi = pi + 1
      ti = ti + 1
    elseif pi <= #p and p:sub(pi, pi) == "*" then
      star = pi
      mark = ti
      pi = pi + 1
    elseif star then
      pi = star + 1
      mark = mark + 1
      ti = mark
    else
      return false
    end
  end
  while pi <= #p and p:sub(pi, pi) == "*" do pi = pi + 1 end
  return pi > #p
end

--------------------------------------------------------------------------------
-- Subject extraction
--------------------------------------------------------------------------------

--- What a rule's pattern is matched against, per tool.
local function subject_for(name, args)
  local key
  if name == "bash" then key = "cmd"
  elseif name == "repl" then key = "code"
  elseif name == "write_file" or name == "edit_file" then key = "path"
  else key = ""
  end

  if key ~= "" and type(args) == "table" and args[key] ~= nil and type(args[key]) == "string" then
    return args[key]
  end

  -- Fall back to compact JSON of args.
  return json_encode(args)
end

--------------------------------------------------------------------------------
-- Rules
--------------------------------------------------------------------------------

local function rule_matches(rule, tool, subject)
  return (rule.tool == "*" or rule.tool == tool) and wildcard_match(rule.pattern, subject)
end

--- Builtin defaults: ask on the mutating surfaces.
local base_rules = {
  { tool = "repl", pattern = "*", action = "ask", provenance = "default" },
  { tool = "bash", pattern = "*", action = "ask", provenance = "default" },
  { tool = "write_file", pattern = "*", action = "ask", provenance = "default" },
  { tool = "edit_file", pattern = "*", action = "ask", provenance = "default" },
}

--------------------------------------------------------------------------------
-- Persistence
--------------------------------------------------------------------------------

local cwd = _phi_approvals_cwd or "."
local config_rules_raw = _phi_approvals_rules or {}
local store_path = _phi_approvals_store_path

-- Determine default store path.
if not store_path then
  local home = os.getenv("HOME") or ""
  local xdg_data = os.getenv("XDG_DATA_HOME")
  if xdg_data then
    store_path = xdg_data .. "/phi/approvals.json"
  elseif home ~= "" then
    store_path = home .. "/.local/share/phi/approvals.json"
  else
    store_path = ".phi-approvals.json"
  end
end

local function store_key(cwd_path)
  -- Canonicalize: resolve symlinks if possible.
  -- Without lfs, we just use the path as-is.
  return cwd_path
end

local function load_store()
  local raw = phi.fs.readfile(store_path)
  if not raw then return {} end
  return json_decode(raw) or {}
end

local function save_store(entries)
  return phi.fs.writefile(store_path, json_encode(entries))
end

local function rules_for(store, cwd_path)
  return store[store_key(cwd_path)] or {}
end

local function append_rule(store, cwd_path, rule)
  local key = store_key(cwd_path)
  if not store[key] then store[key] = {} end
  table.insert(store[key], rule)
  save_store(store)
end

local function clear_rules(store, cwd_path)
  local key = store_key(cwd_path)
  store[key] = nil
  save_store(store)
end

--------------------------------------------------------------------------------
-- Gate state
--------------------------------------------------------------------------------

-- Build config rules with defaults for missing fields.
local config_rules = {}
for _, r in ipairs(config_rules_raw) do
  table.insert(config_rules, {
    tool = r.tool or "*",
    pattern = r.pattern or "*",
    action = r.action or "ask",
    provenance = "config",
  })
end

-- Load persisted rules.
local store = load_store()
local persisted = rules_for(store, cwd)
local session_rules = {}

--- All rules in evaluation order (later wins).
local function all_rules()
  local all = {}
  for _, r in ipairs(base_rules) do table.insert(all, r) end
  for _, r in ipairs(config_rules) do table.insert(all, r) end
  for _, r in ipairs(persisted) do
    table.insert(all, { tool = r.tool, pattern = r.pattern, action = r.action, provenance = "saved" })
  end
  for _, r in ipairs(session_rules) do
    table.insert(all, { tool = r.tool, pattern = r.pattern, action = r.action, provenance = "session" })
  end
  return all
end

--- Last matching rule wins; unmatched calls proceed (allow).
local function evaluate(tool, subject)
  local matched = nil
  for _, r in ipairs(all_rules()) do
    if rule_matches(r, tool, subject) then
      matched = r
    end
  end
  return matched and matched.action or "allow"
end

local function always_allow(tool)
  local rule = { tool = tool, pattern = "*", action = "allow" }
  table.insert(persisted, rule)
  append_rule(store, cwd, rule)
end

local function reset()
  persisted = {}
  session_rules = {}
  clear_rules(store, cwd)
end

--------------------------------------------------------------------------------
-- Helpers
--------------------------------------------------------------------------------

local function subject_preview(subject)
  -- Count chars (not bytes). Lua's # gives bytes, but for ASCII this is fine.
  -- For multi-byte safety, we use a simple byte-count heuristic (matches Rust's
  -- char count closely enough for a preview).
  if #subject <= SUBJECT_PREVIEW_CHARS then
    return subject
  end
  return subject:sub(1, SUBJECT_PREVIEW_CHARS) .. "\n… (truncated)"
end

local function block(reason)
  return { block = true, reason = reason }
end

--------------------------------------------------------------------------------
-- Gate handler
--------------------------------------------------------------------------------

local function gate(name, args)
  -- Extract args from the event payload structure.
  -- The event handler receives the full payload: { type = "ToolCall", data = { name, args } }
  -- But by the time this function is called, name and args are already extracted.
  local subject = subject_for(name, args)
  local action = evaluate(name, subject)

  if action == "allow" then
    return nil
  elseif action == "deny" then
    return block("'" .. name .. "' is denied by an approvals rule. Do not retry the same call; "
      .. "adjust your approach or ask the user.")
  else -- "ask"
    if not phi.ui.has_ui() then
      return block("'" .. name .. "' requires user approval but no interactive UI is attached "
        .. "— denied. Do not retry; the user must run this interactively or "
        .. "add an allow rule to `approvals` in config.lua.")
    end

    local idx, outcome = phi.ui.ask {
      title = "Allow " .. name .. "?",
      body = subject_preview(subject),
      options = {
        "Allow once",
        { label = "Always allow " .. name, detail = "persisted for this project" },
        "Deny",
      },
      wait = ASK_WAIT,
    }

    if outcome == "answer" and idx == 1 then
      -- Allow once.
      return nil
    elseif outcome == "answer" and idx == 2 then
      -- Always allow: persist the rule.
      always_allow(name)
      return nil
    else
      -- Denied, dismissed, timed out, or no UI.
      return block("The user denied this '" .. name .. "' call. Do not retry the same call; "
        .. "ask the user how to proceed.")
    end
  end
end

--------------------------------------------------------------------------------
-- Event + command registration
--------------------------------------------------------------------------------

phi.on("tool_call", function(event)
  -- event is the payload: { type = "ToolCall", data = { name = "...", args = {...} } }
  local data = event.data or event
  local name = data.name
  local args = data.args or {}
  if not name then return nil end
  return gate(name, args)
end, { timeout = HANDLER_TIMEOUT })

phi.register_command("approvals", {
  description = "list tool approval rules; `reset` clears saved answers",
  args_hint = "[reset]",
  completions = { "reset" },
  context = "session",
  handler = function(args)
    local arg = args:match("^%s*(.-)%s*$")
    if arg == "" then
      local lines = { "approval rules (last match wins; unmatched calls proceed):" }
      for _, r in ipairs(all_rules()) do
        local prov = r.provenance or "?"
        table.insert(lines, string.format("  [%s] %s %s %s", prov, r.action, r.tool, r.pattern))
      end
      return table.concat(lines, "\n")
    elseif arg == "reset" then
      reset()
      return "cleared saved approval answers for this project"
    else
      error("unknown argument '" .. arg .. "' (usage: /approvals [reset])")
    end
  end,
})
