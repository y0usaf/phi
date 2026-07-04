-- The `phi:help` overlay — the host's keybinding/command reference.
--
-- The host supplies the content as a `HelpPayload` built at open time, so
-- live keymap overrides are reflected; this overlay owns scrolling and
-- rendering.
--
-- The payload arrives as a table: { kind = "help", entries = { ... } }
-- where each entry is { heading = "..." }, { keys = "...", description = "..." },
-- or {} (blank line).

local function overlay_rect(cols, rows)
    cols = math.max(cols, 1)
    rows = math.max(rows, 1)
    local width = math.min(math.max(cols, 1), 88)
    local height = math.min(math.max(rows, 1), 20)
    return {
        col = (cols - width) / 2,
        row = (rows - height) / 2,
        cols = width,
        rows = height,
    }
end

local function visible_rows(cols, rows)
    local rect = overlay_rect(cols, rows)
    -- inner = rect inset(2, 1): col+2, row+1, cols-4, rows-2
    local inner_rows = rect.rows - 2
    return math.max(inner_rows - 2, 0)
end

local function clamp_scroll(scroll, total, visible)
    return math.min(scroll, math.max(total - visible, 0))
end

phi.register_overlay("phi:help", {
    description = "Keybinding and command reference",

    init = function(payload)
        local entries = {}
        if payload and payload.entries then
            for _, e in ipairs(payload.entries) do
                if e.heading then
                    entries[#entries + 1] = { type = "heading", text = e.heading }
                elseif e.keys then
                    entries[#entries + 1] = { type = "item", keys = e.keys, description = e.description }
                else
                    entries[#entries + 1] = { type = "blank" }
                end
            end
        end
        if #entries == 0 then
            entries[1] = { type = "heading", text = "No help available" }
        end
        return { entries = entries, scroll = 0 }
    end,

    render = function(state, ctx)
        local cols = ctx.cols
        local rows = ctx.rows
        local rect = overlay_rect(cols, rows)
        local inner_col = rect.col + 2
        local inner_row = rect.row + 1
        local inner_cols = math.max(rect.cols - 4, 0)
        local inner_rows = math.max(rect.rows - 2, 0)

        local list_row = inner_row + 2
        local list_rows = math.max(inner_row + inner_rows - list_row, 0)

        local ops = {
            -- Border box
            { op = "box", col = rect.col, row = rect.row, cols = rect.cols, rows = rect.rows,
              fg = "fg", bg = "surface_raised", border = "statusbar_bg" },
        }

        if rect.cols <= 2 or rect.rows <= 2 then
            return ops
        end

        -- Title line: " HELP " on the left, " ctrl+g/Esc/q close " on the right
        local text_width = math.max(inner_cols - 1, 0)
        ops[#ops + 1] = { op = "text", col = inner_col, row = inner_row,
                          max_cols = text_width, text = " HELP ", bold = true, fg = "accent_2",
                          bg = "statusbar_bg" }
        ops[#ops + 1] = { op = "text", col = inner_col, row = inner_row,
                          max_cols = text_width, text = " ctrl+g/Esc/q close ", fg = "muted",
                          bg = "statusbar_bg" }

        if list_rows == 0 then
            return ops
        end

        -- Clamp scroll to content
        state.scroll = clamp_scroll(state.scroll, #state.entries, list_rows)

        -- Render visible entries
        local row = list_row
        for i = state.scroll + 1, math.min(state.scroll + list_rows, #state.entries) do
            local e = state.entries[i]
            if e.type == "blank" then
                -- skip
            elseif e.type == "heading" then
                ops[#ops + 1] = { op = "text", col = inner_col, row = row,
                                  max_cols = text_width, text = e.text, bold = true,
                                  fg = "accent_2", bg = "statusbar_bg" }
            elseif e.type == "item" then
                local keys_w = math.min(24, text_width)
                ops[#ops + 1] = { op = "text", col = inner_col, row = row,
                                  max_cols = keys_w, text = e.keys, fg = "accent_2" }
                local desc_w = math.max(text_width - 25, 0)
                ops[#ops + 1] = { op = "text", col = inner_col + 25, row = row,
                                  max_cols = desc_w, text = e.description, fg = "muted" }
            end
            row = row + 1
        end

        -- Scrollbar
        ops[#ops + 1] = {
            op = "scrollbar",
            col = inner_col + inner_cols - 1,
            row = list_row,
            rows = list_rows,
            visible = list_rows,
            total = #state.entries,
            scroll = state.scroll,
            fg = "border",
            bg = "bg",
            thumb_fg = "accent_2",
        }

        return ops
    end,

    handle_key = function(state, key, cols, rows)
        -- Ctrl+C / Ctrl+G: close
        if key.ctrl then
            if key.code == "char" and (key.char == "c" or key.char == "g") then
                return "close"
            end
            return nil
        end
        -- Shift+G: scroll to bottom
        if key.shift and key.code == "char" and key.char == "G" then
            local vis = visible_rows(cols, rows)
            state.scroll = clamp_scroll(math.huge, #state.entries, vis)
            return nil
        end
        -- Navigation
        if key.code == "esc" or (key.code == "char" and key.char == "q") then
            return "close"
        end
        local vis = visible_rows(cols, rows)
        if key.code == "up" or (key.code == "char" and key.char == "k") then
            state.scroll = math.max(state.scroll - 1, 0)
        elseif key.code == "down" or (key.code == "char" and key.char == "j") then
            state.scroll = clamp_scroll(state.scroll + 1, #state.entries, vis)
        elseif key.code == "pageup" then
            state.scroll = math.max(state.scroll - vis, 0)
        elseif key.code == "pagedown" then
            state.scroll = clamp_scroll(state.scroll + vis, #state.entries, vis)
        elseif key.code == "home" or (key.code == "char" and key.char == "g") then
            state.scroll = 0
        elseif key.code == "end" then
            state.scroll = clamp_scroll(math.huge, #state.entries, vis)
        end
        return nil
    end,

    handle_scroll = function(state, delta, cols, rows)
        local vis = visible_rows(cols, rows)
        state.scroll = clamp_scroll(state.scroll + delta, #state.entries, vis)
    end,
})
