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

local VALID_ACTIONS = { allow = true, deny = true, ask = true }

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
  return phi.json.encode(args)
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
  -- Canonical path (symlinks resolved) so two spellings of the same
  -- project share one rule set; the raw path is the fallback for paths
  -- that no longer resolve.
  return phi.fs.canonicalize(cwd_path) or cwd_path
end

local function load_store()
  local raw = phi.fs.readfile(store_path)
  if not raw then return {} end
  local decoded = phi.json.decode(raw)
  if type(decoded) ~= "table" then return {} end
  return decoded
end

local function save_store(entries)
  local parent = store_path:match("^(.*)/[^/]+$")
  if parent then phi.fs.mkdir_p(parent) end
  return phi.fs.writefile(store_path, phi.json.encode(entries))
end

local function rules_for(store, cwd_path)
  return store[store_key(cwd_path)] or {}
end

--- Read-modify-write: reload the store from disk before mutating so a
--- concurrent phi session's saved answers aren't clobbered by our stale
--- startup snapshot. (Best effort — there's no cross-process lock.)
local function append_rule(cwd_path, rule)
  local store = load_store()
  local key = store_key(cwd_path)
  if not store[key] then store[key] = {} end
  table.insert(store[key], rule)
  save_store(store)
end

local function clear_rules(cwd_path)
  local store = load_store()
  store[store_key(cwd_path)] = nil
  save_store(store)
end

--------------------------------------------------------------------------------
-- Gate state
--------------------------------------------------------------------------------

-- Build config rules with defaults for missing fields. A typo'd action
-- must fail registration loudly, not silently weaken (or harden) the gate.
local config_rules = {}
for i, r in ipairs(config_rules_raw) do
  local action = r.action or "ask"
  if not VALID_ACTIONS[action] then
    error(("approvals: rule %d has unknown action '%s' (expected allow|deny|ask)")
      :format(i, tostring(action)))
  end
  table.insert(config_rules, {
    tool = r.tool or "*",
    pattern = r.pattern or "*",
    action = action,
    provenance = "config",
  })
end

-- Load persisted rules (startup snapshot; writes re-read the store).
local persisted = rules_for(load_store(), cwd)
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
  append_rule(cwd, rule)
end

local function reset()
  persisted = {}
  session_rules = {}
  clear_rules(cwd)
end

--------------------------------------------------------------------------------
-- Helpers
--------------------------------------------------------------------------------

local function subject_preview(subject)
  -- ≤ N bytes is always ≤ N chars: fast path.
  if #subject <= SUBJECT_PREVIEW_CHARS then
    return subject
  end
  -- Cut on a character boundary — a byte-index sub() can split a UTF-8
  -- sequence and hand the host an invalid string.
  local cut = utf8.offset(subject, SUBJECT_PREVIEW_CHARS + 1)
  if not cut then
    return subject
  end
  return subject:sub(1, cut - 1) .. "\n… (truncated)"
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
