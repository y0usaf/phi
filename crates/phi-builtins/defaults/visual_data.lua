-- Phi's default spinner and thinking expressions.
--
-- The braille-dot spinner and quiet thinking labels that ship out-of-the-box.
-- Registered through `phi.register_spinner` and `phi.register_thinking_expression`
-- like any user extension would — this is where the frontend's default animation
-- and status labels come from.
--
-- The theme resolver and text effects (gradient, shimmer, equalizer) stay in
-- Rust (`DefaultVisualExtension`) because they call `phi_tui::generate_theme()`,
-- `phi_tui::gradient_text()`, etc. — native rendering functions not exposed to
-- the Lua extension API. A user who wants a static theme palette can register
-- one from Lua; the adaptive resolver is the honest exception.

-- ── Default spinner (braille dots) ───────────────────────────────────────
-- 10-frame braille sequence, 80ms per frame (12.5 FPS, 800ms full cycle).
phi.register_spinner("phi:default", {
    frames = {
        "\u{280B}", "\u{2819}", "\u{2839}", "\u{2838}", "\u{283C}",
        "\u{2834}", "\u{2826}", "\u{2827}", "\u{2807}", "\u{280F}",
    },
    frame_interval_ms = 80,
})

-- ── Default thinking expressions ─────────────────────────────────────────
-- One or two quiet words each: the gradient animation carries the visual
-- weight, the label is just a status word. Cycles every 2 seconds.
phi.register_thinking_expression("phi:default", {
    expressions = { "thinking...", "reasoning...", "pondering...", "working..." },
    tool_expression = "running...",
    expression_interval_ms = 2000,
})

-- ── Default style (WS10) ─────────────────────────────────────────────────
-- Phi's own look as a switchable style pack. The bundle is deliberately
-- empty: absent members fall back to the base registrations above (and the
-- Rust theme resolver), so `/style phi` means "the defaults" and switching
-- back from another pack restores them.
