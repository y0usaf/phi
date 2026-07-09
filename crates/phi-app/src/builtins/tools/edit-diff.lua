-- edit-diff.ts -- matching, line endings, and diff details.
local function normalize_to_lf(text)
  return (text:gsub("\r\n", "\n"):gsub("\r", "\n"))
end

local function detect_line_ending(content)
  local crlf, lf = content:find("\r\n", 1, true), content:find("\n", 1, true)
  if crlf and lf and crlf <= lf then return "\r\n" end
  return "\n"
end

local function restore_line_endings(text, ending)
  if ending == "\r\n" then return (text:gsub("\n", "\r\n")) end
  return text
end

local function normalize_for_fuzzy_match(text)
  text = pi.text.nfkc(text)
  local lines = split(text, "\n")
  for i = 1, #lines do lines[i] = lines[i]:gsub("%s+$", "") end
  text = table.concat(lines, "\n")
  for _, q in ipairs({ "‘", "’", "‚", "‛" }) do text = text:gsub(q, "'") end
  for _, q in ipairs({ "“", "”", "„", "‟" }) do text = text:gsub(q, '"') end
  for _, d in ipairs({ "‐", "‑", "‒", "–", "—", "―", "−" }) do text = text:gsub(d, "-") end
  for _, s in ipairs({ " ", " ", " ", " ", " ", " ", " ", " ", " ", " ", " ", " ", "　" }) do text = text:gsub(s, " ") end
  return text
end

local function fuzzy_find_text(content, old_text)
  local index = content:find(old_text, 1, true)
  if index then return { found = true, index = index, length = #old_text, fuzzy = false } end
  local haystack, needle = normalize_for_fuzzy_match(content), normalize_for_fuzzy_match(old_text)
  index = haystack:find(needle, 1, true)
  if not index then return { found = false, fuzzy = false } end
  return { found = true, index = index, length = #needle, fuzzy = true }
end

local function count_occurrences(content, old_text)
  local haystack, needle = normalize_for_fuzzy_match(content), normalize_for_fuzzy_match(old_text)
  local count, start = 0, 1
  while true do
    local i = haystack:find(needle, start, true)
    if not i then return count end
    count, start = count + 1, i + #needle
  end
end

local function indexed_message(path, i, total, single, multi)
  if total == 1 then return single:format(path) end
  return multi:format(i - 1, path)
end

local function apply_edits_to_normalized_content(content, edits, path)
  local normalized, use_fuzzy = {}, false
  for i, edit in ipairs(edits) do
    normalized[i] = { oldText = normalize_to_lf(edit.oldText), newText = normalize_to_lf(edit.newText) }
    if normalized[i].oldText == "" then
      error(indexed_message(path, i, #edits, "oldText must not be empty in %s.", "edits[%d].oldText must not be empty in %s."), 0)
    end
    if fuzzy_find_text(content, normalized[i].oldText).fuzzy then use_fuzzy = true end
  end
  local base = use_fuzzy and normalize_for_fuzzy_match(content) or content
  local matches = {}
  for i, edit in ipairs(normalized) do
    local match = fuzzy_find_text(base, edit.oldText)
    if not match.found then
      error(indexed_message(path, i, #edits, "Could not find the exact text in %s. The old text must match exactly including all whitespace and newlines.", "Could not find edits[%d] in %s. The oldText must match exactly including all whitespace and newlines."), 0)
    end
    local occurrences = count_occurrences(base, edit.oldText)
    if occurrences > 1 then
      if #edits == 1 then error(("Found %d occurrences of the text in %s. The text must be unique. Please provide more context to make it unique."):format(occurrences, path), 0) end
      error(("Found %d occurrences of edits[%d] in %s. Each oldText must be unique. Please provide more context to make it unique."):format(occurrences, i - 1, path), 0)
    end
    matches[#matches + 1] = { editIndex = i - 1, index = match.index, length = match.length, newText = edit.newText }
  end
  table.sort(matches, function(a, b) return a.index < b.index end)
  for i = 2, #matches do
    local previous, current = matches[i - 1], matches[i]
    if previous.index + previous.length > current.index then
      error(("edits[%d] and edits[%d] overlap in %s. Merge them into one edit or target disjoint regions."):format(previous.editIndex, current.editIndex, path), 0)
    end
  end
  local result = base
  for i = #matches, 1, -1 do
    local m = matches[i]
    result = result:sub(1, m.index - 1) .. m.newText .. result:sub(m.index + m.length)
  end
  if result == base then
    if #edits == 1 then error("No changes made to " .. path .. ". The replacement produced identical content. This might indicate an issue with special characters or the text not existing as expected.", 0) end
    error("No changes made to " .. path .. ". The replacements produced identical content.", 0)
  end
  return base, result
end

local function generate_diff_string(old_content, new_content, context)
  context = context or 4
  local old, new = split(old_content, "\n"), split(new_content, "\n")
  local n, changed, first = math.max(#old, #new), {}, nil
  for i = 1, n do if old[i] ~= new[i] then changed[i], first = true, first or i end end
  local show = {}
  for i in pairs(changed) do for j = math.max(1, i - context), math.min(n, i + context) do show[j] = true end end
  local width, out, last = #tostring(n), {}, 0
  for i = 1, n do
    if show[i] then
      if last > 0 and i > last + 1 then out[#out + 1] = " " .. string.rep(" ", width) .. " ..." end
      if changed[i] then
        if old[i] ~= nil then out[#out + 1] = "-" .. string.format("%" .. width .. "d", i) .. " " .. old[i] end
        if new[i] ~= nil then out[#out + 1] = "+" .. string.format("%" .. width .. "d", i) .. " " .. new[i] end
      else
        out[#out + 1] = " " .. string.format("%" .. width .. "d", i) .. " " .. old[i]
      end
      last = i
    end
  end
  return table.concat(out, "\n"), first
end

local function generate_unified_patch(path, old_content, new_content)
  local old, new = split(old_content, "\n"), split(new_content, "\n")
  if old[#old] == "" then table.remove(old) end
  if new[#new] == "" then table.remove(new) end
  local out = { "--- " .. path, "+++ " .. path, ("@@ -1,%d +1,%d @@"):format(#old, #new) }
  for _, line in ipairs(old) do out[#out + 1] = "-" .. line end
  for _, line in ipairs(new) do out[#out + 1] = "+" .. line end
  return table.concat(out, "\n") .. "\n"
end
