# PHASE 4: DURABILITY + OBSERVABILITY

## Part 1: Durable Entry Types

### Overview

The existing JSONL session schema uses entry types: `session`, `message`, `repl`, `exchange_start`, `exchange_end`, and `leaf`. Phase 4 extends this schema with operation-level, turn-level, tool-call-level, queue-level, and pending-write-level durable entries. Every append remains flushed + `sync_data`'d — disk first, memory second.
