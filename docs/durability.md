# Durability: the log is the session

Phi's durability story is one idea applied consistently: **the append-only
JSONL session file is the single source of truth.** Memory is a cache of the
log, never the reverse. Anything not yet appended is, by definition, work phi
is willing to lose.

This is event sourcing expressed in Unix dialect — one flat file of
newline-delimited facts, readable by `cat`, `grep`, `jq`, `tail -f`, and
every tool ever written. The same durability semantics that systems like
[absurd](https://github.com/earendil-works/absurd) build on Postgres, phi
builds on a plain file, because phi is local-first and single-user: the
substrate that fits is files, not daemons.

## The five rules

1. **The JSONL file is the single source of truth.**
   Restore (`branch_restore`) rebuilds the live REPL environment by
   replaying the log. There is no other recovery path.

2. **Every expensive effect is logged the moment it completes, not when the
   exchange ends.** A root completion or REPL execution that cost seconds
   and money is an asset; assets go to disk. User prompts are appended at
   send; agent and shell REPL executions at `CodeExecEnd`; assistant
   responses when the run completes.

3. **Append-only, never rewrite.** Branching, undo, forking, and audit all
   fall out of one invariant: lines are immutable and ordered. Editing
   history means writing a new file (`fork`), never mutating an old one.

4. **Replay is the only recovery mechanism.** Startup *is* recovery, every
   time. No special crash-repair path exists, so the normal path is the
   tested path. Replay does **not** re-execute recorded code: restore
   injects each prior exchange's transcript and REPL outputs as `history_N`
   variables, so side effects are never re-run and completed work is never
   re-billed. The live Python heap itself is not checkpointed — variables
   other than the injected history are gone after a restart — and that
   trade is accepted, not hidden: replaying side-effecting code to rebuild
   a heap would be less honest than admitting the heap is ephemeral.

5. **Plain text, tagged records, tolerant reader.** Every line is a JSON
   object with a `type` tag. Unknown types are skipped (old binaries read
   new files), malformed lines are skipped (one bad write never loses a
   session), and a truncated final line is treated as the expected crash
   signature of an append-only log, not corruption. The format grows
   without migrations.

## The record types

Every line is a tagged fact. The current vocabulary:

- `session` — header: id, timestamp, cwd, optional parent session (forks).
- `message` — user/assistant entry with `id`/`parent_id` (the branch tree);
  assistant entries carry per-turn token/cost usage.
- `repl` — one completed REPL execution: code, stdout/stderr, duration,
  kind (agent/shell), plus the **tool calls** (name, args, result, error)
  and **sub-LLM calls** (model, prompt preview, error) that ran inside it,
  so a resumed session re-renders the same nested cards the user saw live.
- `exchange_start` / `exchange_end` — durable turn boundaries. An
  `exchange_start` without its `exchange_end` is the crash signature of a
  run that died mid-turn; load detects it, the UI says so, and the
  exchange is durably closed as `interrupted`. `exchange_end` records the
  terminal status: `completed`, `failed`, or `interrupted`.
- `leaf` — durable branch-tip marker. Tree navigation (`set_leaf`) is a
  state transition, so it is logged like any other fact; resume returns to
  the branch the user was actually on, not the newest entry.

Unknown types are skipped by the tolerant reader, so the vocabulary can
grow without migrations.

## Write policy

A line is a fact only after it reaches disk: every append is flushed and
`sync_data`'d. Sessions append at human-interaction rates, so the fsync cost
is negligible next to the LLM and REPL work it protects.

Three corollaries, applied consistently:

- **Disk first, memory second.** In-memory state mutates only after the
  write succeeds, so a failed append can never leave memory ahead of the
  log.
- **The header is as durable as every append.** The first write syncs the
  file *and* fsyncs the directory, so the session's name itself survives a
  crash.
- **Forks are atomic.** `fork` writes the new session to a temp name and
  renames it into place; a crash mid-fork leaves no half-copied session
  masquerading as a real one.

## Failure model

| Fault | Outcome |
|---|---|
| Provider 429/5xx/transport error | Retried in `ProviderLm` with exponential backoff, only while nothing has streamed; never duplicates visible output |
| Crash between steps of an exchange | Prompt + completed REPL steps are on disk; resume replays them, losing at most the in-flight step; the open `exchange_start` marks the turn as interrupted |
| Crash mid-append | Torn final line dropped by the tolerant reader |
| Non-UTF8 corruption | Decoded lossily; damage is contained to the affected lines |
| Replayed side effects | None: restore injects recorded history instead of re-executing code (rule 4) |

## What phi deliberately does not do

- **No token-level logging.** Checkpoints happen at *step* granularity
  (completed results). Logging deltas would turn the log into sludge.
- **No daemon.** Postgres-backed durable execution (absurd et al.) is the
  right tool for unattended multi-worker workflows; phi's interactive loop
  gets the same semantics from a file. If phi grows headless background
  agents, that becomes an optional backend behind a trait — never a
  requirement for the interactive path.
- **No workflow engine.** A handful of record types and a replay loop. The
  moment this grows task queues and schedulers, it has failed.

## Future work (in spirit, when needed)

- Logging full sub-LLM results as replayable steps (today the log records
  that each call happened, its model, preview, and outcome), so an
  interrupted turn could be resumed mid-exchange serving completed calls
  from the log instead of re-billing (absurd's checkpoint-by-step trick).
- Checkpoint/compaction cut-points so very long sessions don't replay the
  full raw log forever.
- Small composable verbs over the format: `phi log`, `phi cost`,
  `phi replay --until` — jq one-liners first, subcommands second. The file
  format is the API.
