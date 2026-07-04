-- The `phi:about` overlay — a simple info panel.
--
-- This is the simplest overlay Phi ships: it demonstrates the extension
-- overlay pipeline with draw ops, palette awareness, and key handling.
-- Toggled by Ctrl+? in the mux, closes on Esc/Enter/q.
--
-- A user extension can register an overlay with the same name to replace
-- this one (last-write-wins).

phi.register_overlay("phi:about", {
    description = "About Phi — version and build info",
    toggle_key = "ctrl+?",

    render = function(state, ctx)
        local cols = ctx.cols
        local rows = ctx.rows
        local width = math.min(math.max(cols, 20), 60)
        local height = math.min(math.max(rows, 6), 12)
        local col = (cols - width) / 2
        local row = (rows - height) / 2

        local ops = {
            -- Border box
            { op = "box", col = col, row = row, cols = width, rows = height,
              fg = "fg", bg = "surface_raised", border = "border" },
            -- Title
            { op = "text", col = col + 2, row = row + 1, text = " Phi ", bold = true, fg = "accent" },
            -- Body
            { op = "text", col = col + 2, row = row + 3, text = "An AI coding agent with an extensible TUI." },
            { op = "text", col = col + 2, row = row + 4, text = "Extensions can register overlays," },
            { op = "text", col = col + 2, row = row + 5, text = "themes, animations, and more." },
            -- Footer
            { op = "text", col = col + 2, row = row + 7, text = "Press Esc/Enter/q to close.", fg = "muted" },
        }
        return ops
    end,

    handle_key = function(state, key, cols, rows)
        if key.code == "esc" or key.code == "enter" then
            return "close"
        end
        if key.code == "char" and (key.char == "q" or key.char == "Q") and not key.ctrl then
            return "close"
        end
        return nil
    end,
})
