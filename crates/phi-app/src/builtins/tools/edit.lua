-- edit.ts -- exact/fuzzy single-file replacement through the public API.
-- Rendering lands with WS6; signal/ctx with WS4.
pi.register_tool({
  name = "edit", label = "edit",
  description = "Edit a single file using exact text replacement. Every edits[].oldText must match a unique, non-overlapping region of the original file. If two changes affect the same block or nearby lines, merge them into one edit instead of emitting overlapping edits. Do not include large unchanged regions just to connect distant changes.",
  promptSnippet = "Make precise file edits with exact text replacement, including multiple disjoint edits in one call",
  promptGuidelines = {
    "Use edit for precise changes (edits[].oldText must match exactly)",
    "When changing multiple separate locations in one file, use one edit call with multiple entries in edits[] instead of multiple edit calls",
    "Each edits[].oldText is matched against the original file, not after earlier edits are applied. Do not emit overlapping or nested edits. Merge nearby changes into one edit.",
    "Keep edits[].oldText as small as possible while still being unique in the file. Do not pad with large unchanged regions.",
  },
  parameters = { type = "object", properties = {
    path = { type = "string", description = "Path to the file to edit (relative or absolute)" },
    edits = { type = "array", items = { type = "object", properties = {
      oldText = { type = "string", description = "Exact text for one targeted replacement." },
      newText = { type = "string", description = "Replacement text for this targeted edit." },
    }, required = { "oldText", "newText" }, additionalProperties = false } },
  }, required = { "path", "edits" }, additionalProperties = false },
  renderShell = "self",
  prepare_arguments = function(params)
    if type(params) ~= "table" then return params end
    if type(params.edits) == "string" then
      local ok, parsed = pcall(pi.json.decode, params.edits)
      if ok and type(parsed) == "table" then params.edits = parsed end
    end
    if type(params.oldText) == "string" and type(params.newText) == "string" then
      local edits = type(params.edits) == "table" and params.edits or {}
      edits[#edits + 1] = { oldText = params.oldText, newText = params.newText }
      params.oldText, params.newText, params.edits = nil, nil, edits
    end
    return params
  end,
  execute = function(_tool_call_id, params)
    local path, edits = params.path, params.edits
    if type(edits) ~= "table" or #edits == 0 then error("Edit tool input is invalid. edits must contain at least one replacement.", 0) end
    local absolute_path = resolve_to_cwd(path)
    return with_file_mutation_queue(absolute_path, function()
      if not pi.fs.exists(absolute_path) then error("Could not edit file: " .. path .. ". Error code: ENOENT.", 0) end
      local raw = utf8_lossy(pi.fs.read_bytes(absolute_path))
      local bom = ""
      if raw:sub(1, 3) == "\239\187\191" then bom, raw = "\239\187\191", raw:sub(4) end
      local ending = detect_line_ending(raw)
      local base, changed = apply_edits_to_normalized_content(normalize_to_lf(raw), edits, path)
      pi.fs.write_file(absolute_path, bom .. restore_line_endings(changed, ending))
      local diff, first = generate_diff_string(base, changed)
      return {
        content = { { type = "text", text = ("Successfully replaced %d block(s) in %s."):format(#edits, path) } },
        details = { diff = diff, patch = generate_unified_patch(path, base, changed), firstChangedLine = first },
      }
    end)
  end,
})
