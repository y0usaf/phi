-- The shipped default agent profile (WS1): the empty bundle.
-- Every member is optional and falls through to the engine defaults,
-- so this is literally `register_agent("rlm", {})` — today's behavior
-- with no special case, and a builtins-less build keeps the engine's
-- built-in identity. A user extension can register a different agent
-- and `/agent <name>` to it.
phi.register_agent("rlm", {})
