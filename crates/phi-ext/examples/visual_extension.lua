-- Example: custom visual extension for Phi
--
-- Place this file in .phi/extensions/custom-theme.lua to override
-- Phi's default theme and spinner with your own.
--
-- The built-in default ("phi:default") is always registered first.
-- To override it, use a different name (e.g. "my-theme") — the mux
-- uses the first registered theme, so load order matters. Config
-- extensions (from <config>/phi/extensions/) are loaded before
-- local extensions (from <cwd>/.phi/extensions/), and the built-in
-- default is registered first of all.

-- Register a custom dark theme
phi.register_theme("my-dark-theme", {
    foreground = {220, 220, 220},
    background = {20, 20, 26},
    muted = {120, 120, 130},
    accent = {100, 200, 220},
    accent_2 = {200, 150, 240},
    border = {50, 50, 60},
    surface = {30, 30, 36},
    surface_raised = {35, 35, 42},
    statusbar_bg = {35, 35, 42},
    running = {100, 255, 100},
    success = {100, 200, 100},
    warning = {240, 200, 80},
    error = {255, 100, 100},
    -- ANSI palette (0-15)
    ansi_0 = {0, 0, 0},
    ansi_1 = {200, 80, 80},
    ansi_2 = {80, 200, 80},
    ansi_3 = {200, 180, 80},
    ansi_4 = {80, 140, 240},
    ansi_5 = {200, 120, 240},
    ansi_6 = {80, 220, 220},
    ansi_7 = {200, 200, 200},
})

-- Register a custom spinner (dotted circle)
phi.register_spinner("my-spinner", {
    frames = {"\u{25CC}", "\u{25CB}", "\u{25CF"},
    frame_interval_ms = 200,
})

-- Register custom thinking expressions
phi.register_thinking_expression("my-expressions", {
    expressions = {
        "reasoning...",
        "analyzing...",
        "connecting ideas...",
        "almost there...",
    },
    tool_expression = "executing...",
    expression_interval_ms = 1500,
})
