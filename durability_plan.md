# DURABILITY: THE LOG IS THE SESSION

## Design

Phi's durability follows the absurd model: a step is a checkpoint, replay
skips done work, and the log records *facts* — not a coordinator's
bookkeeping. The JSONL session file is the single source of truth; memory
is a cache of the log, never the reverse. See `docs/durability.md` for the
full rules and record types.

## What lives in the log

The session's own record types are the step-level checkpoints:

- `session` — header: id, timestamp, cwd, optional parent session (forks).
- `message` — user/assistant entry with `id`/`parent_id` (the branch tree);
  assistant entries carry per-turn token/cost usage.
- `repl` — one completed REPL execution: code, stdout/stderr, duration,
  kind (agent/shell), plus tool calls and sub-LLM calls that ran inside it,
  so a resumed session re-renders the same nested cards the user saw live.
- `exchange_start` / `exchange_end` — durable turn boundaries. An
  `exchange_start` without its `exchange_end` is the crash signature of a
  run that died mid-turn.
- `leaf` — durable branch-tip marker. Tree navigation is a state
  transition, so it is logged like any other fact.

That's it. `SessionStore::load` is the only reader: it parses each line,
skips unknown/malformed entries (forward-compatible), and reconstructs the
branch tree, leaf, and interrupted-exchange state in one pass. There is no
separate durable layer, no `RecoveryReducer`, no `RecoveryReport`.

## What was pruned (and why)

Earlier drafts had operation/turn/tool-call nesting, a durable work queue
(`QueueEnqueued`/`QueueConsumed`), and a pending-write WAL
(`PendingWriteEnqueued`/`PendingWriteApplied`) living in the log. That was
a workflow engine in disguise — exactly what `docs/durability.md` warns
against: *"The moment this grows task queues and schedulers, it has
failed."*

The first prune removed:

- `OperationStart`/`OperationFinish`/`OperationInterrupted` — nested
  distributed-tracing hierarchy with no recovery value for an interactive
  single-user agent.
- `TurnStart`/`TurnFinish` — finer-grained tracking within exchanges that
  the exchange boundaries already cover.
- `ToolCallStart`/`ToolCallFinish` — tool calls are already recorded inline
  in `repl` entries.
- `QueueEnqueued`/`QueueConsumed` — the harness queues (steer/follow-up/
  next-turn) are in-memory; log-restoring them was the harness recording its
  own writes, not reading external state.
- `PendingWriteEnqueued`/`PendingWriteApplied` — a WAL for file mutations
  that the `repl` entry already records (completed results only, no
  speculative in-flight state).
- `UnfinishedOperation`/`RestoredQueueMessage`/`PendingWrite` — reducer
  state structs that existed only to support the above.

The second prune folded the remaining durable layer (`durable.rs`) back
into the session store. The `DurableEntry` enum, `RecoveryReducer`,
`reduce_log`, `RecoveryReport`, `ModelChange`, and `append_durable` were
all removed:

- `DurableEntry::Leaf` duplicated `LeafRecord` already in `session.rs`;
  `SessionStore::load` already reduces leaf records.
- `DurableEntry::ModelChange` was never written in production — no
  `append_durable` call sites existed outside tests.
- `RecoveryReport::current_leaf` was redundant with `SessionStore::leaf()`,
  which the TUI already calls directly.
- `harness::recover()` had zero production callers.
- `append_durable()` (raw JSON line writer) had zero production callers.

## Future work

If phi grows headless background agents that need cross-worker durability,
that becomes an optional SQLite/Postgres backend behind a trait — never a
requirement for the interactive path, and never something that lives in
the JSONL log.
