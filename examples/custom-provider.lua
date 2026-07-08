-- custom-provider: register a custom model provider via the extension
-- surface (DESIGN.md WS4: every provider arrives via register_provider --
-- even the first-party catalog is builtins/providers.lua registered
-- through this same door). This is the conformance exerciser for
-- phi.register_provider — it grafts the provider's models onto the
-- registry, and an inline api_key becomes a runtime-only auth override
-- (never persisted to auth.json).
--
-- Install by copying into ~/.config/phi/extensions/ (loads everywhere)
-- or <project>/.phi/extensions/ (loads after you trust the project).
--
-- Pattern 1 — a generic OpenAI-compatible router (OpenRouter, 302.ai,
-- Neuralwatt, self-hosted vLLM, etc.). The api defaults to
-- "openai-completions", so any endpoint that speaks /chat/completions works:

phi.register_provider("openrouter", {
  base_url = "https://openrouter.ai/api/v1",
  api_key = os.getenv("OPENROUTER_API_KEY"),
  models = {
    { id = "anthropic/claude-sonnet-4", name = "Claude Sonnet 4 (OpenRouter)",
      reasoning = false, context_window = 200000, max_tokens = 8192 },
    { id = "google/gemini-2.5-pro", name = "Gemini 2.5 Pro (OpenRouter)",
      reasoning = true, context_window = 1000000, max_tokens = 65536 },
  }
})

-- Pattern 2 — a provider that speaks the Anthropic Messages API (e.g. the
-- Vercel AI Gateway, or a proxy in front of Anthropic). Set api explicitly:

phi.register_provider("vercel", {
  base_url = "https://ai-gateway.vercel.sh/v1",
  api = "anthropic-messages",
  api_key = os.getenv("AI_GATEWAY_API_KEY"),
  models = {
    { id = "claude-sonnet-4-20250514", name = "Claude Sonnet 4 (Vercel)",
      reasoning = true, context_window = 200000, max_tokens = 16384 },
  }
})

-- Pattern 3 — override an existing provider's base_url to route through a
-- proxy. Re-registering a name replaces the prior entry, so this updates
-- rather than errors:

-- phi.register_provider("anthropic", {
--   base_url = "https://my-proxy.example.com",
-- })

-- After loading this extension, run:
--   phi --list-models          # shows openrouter/* and vercel/* models
--   phi --model openrouter/anthropic/claude-sonnet-4 "hello"
--   /model openrouter/google/gemini-2.5-pro   (in the chat TUI)
