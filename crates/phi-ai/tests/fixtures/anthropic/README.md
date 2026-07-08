# Anthropic protocol fixtures

Replay fixtures for `protocols::anthropic` against the spec
(`ref/pi` @ `c5582102`, pi v0.79.0, `providers/anthropic.ts`).

Provenance:

- `replay_basic.sse` — hand-built to Anthropic's documented
  `/v1/messages` streaming shapes (message_start → content blocks →
  message_delta → message_stop, including `ping`, `signature_delta`,
  and split `input_json_delta` frames), matching the exact event set
  `anthropic.ts` consumes. **Pending:** replacement with a transcript
  captured live from pi v0.79.0 once credentials are available (tracked
  in PLAN.md).
- `replay_basic.message.json` / `replay_basic.events.json` — expected
  final `AssistantMessage` and event sequence, hand-derived by executing
  the spec's `streamAnthropic` handler logic over the transcript.
  Events are recorded without their `partial`/`message` snapshots;
  `usage.cost` is zeroed in the fixture and computed by the harness via
  `calculate_cost` (cost math is pinned separately in `phi-ai-types`).
- `params_apikey.request.json` — expected `MessageCreateParamsStreaming`
  body, hand-derived from `buildParams`/`convertMessages`/`convertTools`
  for the full-featured request in `tests/anthropic.rs` (system + cache
  control, same-model signed thinking replay, tool result folding, tool
  cache control, adaptive thinking + effort, metadata filtering,
  tool_choice).
