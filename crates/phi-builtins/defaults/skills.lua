-- Phi Skills: SKILL.md discovery + <available_skills> system-prompt injection.
--
-- Scans default locations and user-configured paths for SKILL.md files,
-- parses their YAML frontmatter (name + description), and appends an
-- <available_skills> block to the system prompt on every before_agent_start
-- event. The model reads the full SKILL.md on-demand when a task matches
-- (progressive disclosure).
--
-- ## Runtime globals (injected before this source runs)
--
--   _phi_skills_cwd              = "/path/to/project"
--   _phi_skills_extra_paths      = { "~/.claude/skills", ".forge/skills" }
--   _phi_skills_include_project  = true|false
--
-- A user extension can override this by registering a handler for
-- "before_agent_start" after this one.
--
-- ## Discovery locations
--
-- Scanned by default (missing dirs are silently skipped):
--   ~/.config/phi/skills/        — global user skills
--   <cwd>/.phi/skills/           — project-level skills (trust-gated)
--   ~/.local/share/phi/skills/   — bundled/installed skills
--
-- Extra paths from config.lua's `skills` field are scanned after the defaults.
-- Paths support ~ expansion and are resolved relative to cwd.
--
-- Discovery runs once, when the extension loads: a SKILL.md added mid-session
-- appears after the next session start, not the next turn. (Intentional — the
-- scan touches the filesystem; before_agent_start must stay cheap.)

--------------------------------------------------------------------------------
-- Constants
--------------------------------------------------------------------------------

local MAX_NAME_LENGTH = 64
local MAX_DESCRIPTION_LENGTH = 1024

--------------------------------------------------------------------------------
-- Path helpers
--------------------------------------------------------------------------------

--- Expand ~ to home and resolve relative paths against cwd.
local function expand_path(raw, cwd)
  if raw:sub(1, 2) == "~/" then
    local home = os.getenv("HOME") or ""
    return home .. "/" .. raw:sub(3)
  elseif raw == "~" then
    return os.getenv("HOME") or "~"
  elseif raw:sub(1, 1) ~= "/" then
    return cwd .. "/" .. raw
  end
  return raw
end

--------------------------------------------------------------------------------
-- Frontmatter parsing
--------------------------------------------------------------------------------

--- Parse YAML frontmatter from markdown content.
-- Returns a table { name = "...", description = "..." } or nil.
local function parse_frontmatter(content)
  -- Trim leading whitespace.
  content = content:match("^%s*(.*)") or content
  if content:sub(1, 3) ~= "---" then return nil end

  -- Skip the opening --- and any blank lines after it.
  local rest = content:sub(4)
  rest = rest:match("^\n(.*)") or rest

  -- Find the closing --- on its own line.
  local end_pos = rest:find("\n%-%-%-")
  if not end_pos then return nil end
  local body = rest:sub(1, end_pos - 1)

  local name, description
  local lines = {}
  for line in body:gmatch("([^\n]*)\n?") do
    table.insert(lines, line)
  end

  local i = 1
  while i <= #lines do
    local line = lines[i]
    local trimmed = line:match("^%s*(.-)%s*$") or line
    if trimmed == "" or trimmed:sub(1, 1) == "#" then
      i = i + 1
    else
      local colon = trimmed:find(":")
      if not colon then
        i = i + 1
      else
        local key = trimmed:sub(1, colon - 1):match("^%s*(.-)%s*$")
        local value = trimmed:sub(colon + 1):match("^%s*(.-)%s*$")

        if value == "" then
          -- Multi-line: collect indented continuation lines.
          local parts = {}
          i = i + 1
          while i <= #lines do
            local next_line = lines[i]
            if next_line:sub(1, 1) == " " or next_line:sub(1, 1) == "\t" then
              local part = next_line:match("^%s*(.-)%s*$")
              if part ~= "" then table.insert(parts, part) end
              i = i + 1
            else
              break
            end
          end
          value = table.concat(parts, " ")
        elseif value:sub(1, 1) == ">" then
          -- Folded scalar (> prefix).
          local first = value:gsub("^>%s*", "")
          local parts = { first }
          i = i + 1
          while i <= #lines do
            local next_line = lines[i]
            if next_line:sub(1, 1) == " " or next_line:sub(1, 1) == "\t" then
              local part = next_line:match("^%s*(.-)%s*$")
              if part ~= "" then table.insert(parts, part) end
              i = i + 1
            else
              break
            end
          end
          value = table.concat(parts, " ")
        else
          i = i + 1
        end

        if key == "name" then name = value
        elseif key == "description" then description = value
        end
      end
    end
  end

  return { name = name, description = description }
end

--------------------------------------------------------------------------------
-- Skill loading
--------------------------------------------------------------------------------

--- Load a single skill from a .md file.
-- Returns a skill table { name, description, path } or nil.
local function load_skill_from_file(filepath, seen_names)
  local raw = phi.fs.readfile(filepath)
  if not raw then return nil end

  local fm = parse_frontmatter(raw)
  if not fm then return nil end

  -- description is required.
  if not fm.description or fm.description:match("^%s*$") then return nil end
  local description = fm.description:match("^%s*(.-)%s*$")

  -- name: frontmatter or parent dir name.
  local parent_dir_name = filepath:match("([^/]+)/[^/]*$") or "unnamed"
  local name = fm.name or parent_dir_name

  -- Validate name (warn but still load, like pi).
  if #name > MAX_NAME_LENGTH then
    io.stderr:write("warning: skill name '" .. name .. "' exceeds "
      .. MAX_NAME_LENGTH .. " characters (loaded anyway)\n")
  end
  if name == "" or not name:match("^[a-z0-9%-]*$") then
    io.stderr:write("warning: skill name '" .. name
      .. "' contains invalid characters (loaded anyway)\n")
  end
  if #description > MAX_DESCRIPTION_LENGTH then
    io.stderr:write("warning: skill '" .. name .. "' description exceeds "
      .. MAX_DESCRIPTION_LENGTH .. " characters (loaded anyway)\n")
  end

  -- Name collision: keep first found, warn.
  if seen_names[name] then
    io.stderr:write("warning: skill name '" .. name
      .. "' collision, keeping first found (at " .. filepath .. ")\n")
    return nil
  end
  seen_names[name] = true

  return { name = name, description = description, path = filepath }
end

--- Recursively load skills from a directory.
-- If the directory contains SKILL.md, load it and return (no recursion).
-- Otherwise, load direct .md children (if include_root_files) and
-- recurse into subdirectories. Hidden entries (starting with .) are skipped.
local function load_skills_from_dir(dir, include_root_files, skills, seen_names)
  local entries = phi.fs.readdir(dir)
  if not entries then return end -- dir doesn't exist

  -- First pass: look for SKILL.md. If found, load it and return.
  for _, entry in ipairs(entries) do
    if entry == "SKILL.md" then
      local filepath = dir .. "/" .. entry
      if not phi.fs.isdir(filepath) then
        local skill = load_skill_from_file(filepath, seen_names)
        if skill then table.insert(skills, skill) end
        return
      end
    end
  end

  -- Second pass: process subdirectories and root .md files.
  for _, entry in ipairs(entries) do
    if entry:sub(1, 1) ~= "." then
      local filepath = dir .. "/" .. entry
      if phi.fs.isdir(filepath) then
        load_skills_from_dir(filepath, false, skills, seen_names)
      elseif include_root_files and entry:sub(-3) == ".md" then
        local skill = load_skill_from_file(filepath, seen_names)
        if skill then table.insert(skills, skill) end
      end
    end
  end
end

--------------------------------------------------------------------------------
-- Discovery
--------------------------------------------------------------------------------

local cwd = _phi_skills_cwd or "."
local extra_paths = _phi_skills_extra_paths or {}
local include_project = _phi_skills_include_project or false

local skills = {}
local seen_names = {}

-- Default locations.
local default_dirs = {}

-- Global: ~/.config/phi/skills/
local xdg_config = os.getenv("XDG_CONFIG_HOME")
if xdg_config then
  table.insert(default_dirs, xdg_config .. "/phi/skills")
else
  local home = os.getenv("HOME") or ""
  if home ~= "" then
    table.insert(default_dirs, home .. "/.config/phi/skills")
  end
end

-- Project: <cwd>/.phi/skills/
if include_project then
  table.insert(default_dirs, cwd .. "/.phi/skills")
end

-- Phi data dir: ~/.local/share/phi/skills/
local xdg_data = os.getenv("XDG_DATA_HOME")
if xdg_data then
  table.insert(default_dirs, xdg_data .. "/phi/skills")
else
  local home = os.getenv("HOME") or ""
  if home ~= "" then
    table.insert(default_dirs, home .. "/.local/share/phi/skills")
  end
end

for _, dir in ipairs(default_dirs) do
  load_skills_from_dir(dir, true, skills, seen_names)
end

-- User-configured extra paths.
for _, raw in ipairs(extra_paths) do
  local expanded = expand_path(raw, cwd)
  if phi.fs.isdir(expanded) then
    load_skills_from_dir(expanded, true, skills, seen_names)
  elseif phi.fs.exists(expanded) and expanded:sub(-3) == ".md" then
    local skill = load_skill_from_file(expanded, seen_names)
    if skill then table.insert(skills, skill) end
  end
end

--------------------------------------------------------------------------------
-- System prompt block
--------------------------------------------------------------------------------

local function available_skills_block(skills_list)
  local entries = {}
  for _, s in ipairs(skills_list) do
    local location = s.path or ""
    table.insert(entries, string.format(
      "  <skill>\n    <name>%s</name>\n    <description>%s</description>\n    <location>%s</location>\n  </skill>",
      s.name, s.description, location
    ))
  end
  return "The following skills provide specialized instructions for specific tasks.\n"
    .. "Read the full skill file when the task matches its description.\n"
    .. "When a skill file references a relative path, resolve it against the skill directory.\n\n"
    .. "<available_skills>\n" .. table.concat(entries, "\n") .. "\n</available_skills>"
end

local skills_block = nil
if #skills > 0 then
  skills_block = available_skills_block(skills)
end

--------------------------------------------------------------------------------
-- Event registration
--------------------------------------------------------------------------------

phi.on("before_agent_start", function(event)
  if skills_block then
    return { system_prompt_append = skills_block }
  end
  return nil
end)
