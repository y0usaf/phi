# Phi Harness Plan: Bridging the Disconnection

## Problem

Phi has an RLM engine (`phi-rlm`) and a runtime registry (`phi-runtime`), but no
orchestration layer between them. The engine runs, the runtime dispatches events,
and the session logs -- but nothing coordinates phases, snapshots, queues, hooks,
recovery, or observability. Every reference harness (Pi's `AgentHarness`, Crush's
`Coordinator`) has this layer. Phi doesn't. That absence is the "disconnected"
feeling: components are islands with no shared lifecycle contract.

## Solution

Introduce a new `phi-harness` crate between `phi-rlm` and the frontends. It owns:

- **Phase state machine** -- idle/turn/compaction/retry with busy-rejection
- **Turn snapshots** -- immutable per-iteration state, replacing direct config reads
- **Save points** -- reconcile state between iterations, flush pending writes
- **Queues** -- steer, follow-up, next-turn, drained at save points
- **Typed hooks** -- observe/on/emit with sequential result chaining
- **Durable operation recovery** -- operation/turn/tool-call entries, log reduction
- **Loop detection** -- tool-call signature hashing, sliding window
- **Observability** -- trace/span tree, stable event names, safe payloads
- **Error normalization** -- `HarnessError` enum with typed codes

---

## Phase 1: Foundation -- Crate, Phases, Snapshots, Errors

### 1.1 New crate: `phi-harness`

**Cargo.toml:**

```toml
[package]
name = "phi-harness"
version = { workspace = true }
edition = { workspace = true }

[dependencies]
phi-rlm = { workspace = true }
phi-session = { workspace = true }
phi-ext = { workspace = true }
phi-runtime = { workspace = true }
phi-ai = { workspace = true }
async-trait = { workspace = true }
thiserror = { workspace = true }
tokio = { workspace = true }
tokio-util = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
uuid = { workspace = true }
jiff = { workspace = true }
```

**Directory structure:**

```
crates/phi-harness/
  Cargo.toml
  src/
    lib.rs          # re-exports, PhiHarness
    phase.rs        # HarnessPhase enum
    snapshot.rs     # TurnState struct
    error.rs        # HarnessError enum
    hooks.rs        # PhiHooks, HookEvent, observe/on/emit
    queue.rs        # SteerQueue, FollowUpQueue, NextTurnQueue
    save_point.rs   # save point logic
    loop_detect.rs  # LoopDetector
    observe.rs      # ObservabilityContext, traceOperation
    durable.rs      # durable entry types, recovery
```

**Dependency graph change:**

```
phi-ai <- phi-rlm <- phi-harness <- phi-app -> phi-tui, phi-cli
                    ^ phi-runtime
          phi-ext <- phi-builtins, phi-lua
```

### 1.2 HarnessPhase enum

Port Pi's phase state machine. Structural operations require `Idle` and set
the phase synchronously before the first `.await`. Calling a structural op
while busy rejects with `HarnessError::Busy`.

```rust
// crates/phi-harness/src/phase.rs

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HarnessPhase {
    Idle,
    Turn,
    Compaction,
    Retry,
}

impl HarnessPhase {
    pub fn is_idle(self) -> bool {
        self == Self::Idle
    }
}
```

**Phase transitions:**

| Operation | Required phase | Sets phase to | Allowed during Turn? |
|---|---|---|---|
| `prompt()` | Idle | Turn | No -- rejects with `Busy` |
| `compact()` | Idle | Compaction | No |
| `navigate_tree()` | Idle | Turn (briefly) | No |
| `steer()` | any | (no change) | Yes |
| `follow_up()` | any | (no change) | Yes |
| `next_turn()` | any | (no change) | Yes |
| `abort()` | any | Idle (after run drains) | Yes |
| `set_model()` | any | (no change, affects next snapshot) | Yes |
| `set_thinking()` | any | (no change, affects next snapshot) | Yes |

Phase is stored behind a `tokio::sync::Mutex<HarnessPhase>` on `PhiHarness`.
Structural ops acquire the lock, check `is_idle()`, set the new phase, then
release the lock before the first `.await` (so queue ops can still proceed).
On completion or error, a `finally`-style guard resets phase to `Idle`.

### 1.3 TurnState struct

An immutable snapshot of everything one RLM iteration needs. Created once
per iteration (and refreshed at save points). All logic for that iteration
reads from the snapshot, not from live `RlmConfig` fields.

```rust
// crates/phi-harness/src/snapshot.rs

#[derive(Clone, Debug)]
pub struct TurnState {
    /// The RLM chat messages for this iteration (system + history + user).
    pub messages: Vec<ChatMessage>,
    /// Resolved system prompt (after hook transforms).
    pub system_prompt: String,
    /// Root model for this iteration.
    pub root_model: Arc<dyn LanguageModel>,
    /// Sub model for sub-calls.
    pub sub_model: Arc<dyn LanguageModel>,
    /// Thinking level for this iteration.
    pub thinking: Option<ThinkingLevel>,
    /// RLM config overrides for this iteration (max_iterations, budget, etc).
    pub config: RlmConfig,
    /// Session ID derived from the session tree.
    pub session_id: String,
    /// Iteration number within the current run.
    pub iteration: usize,
    /// Tools section for the system prompt.
    pub tools_section: Option<String>,
    /// Context payload (the original `context` variable).
    pub context: ContextPayload,
}
```

**How it replaces direct config reads:**

Currently `run_loop()` in `engine.rs` reads from `self.config` (the `Rlm`'s
config) on every iteration. With `TurnState`, the harness creates a snapshot
before each iteration and passes it to the loop. Config changes made mid-run
(via `set_model()`, `set_thinking()`, etc.) update the *harness config* but
don't mutate the current snapshot. At the next save point, a fresh snapshot
is created from the updated harness config, so changes take effect on the
next iteration -- cleanly, deterministically.

### 1.4 HarnessError enum

Normalize all public harness failures to a typed error with codes. Subsystem
errors preserved as `source`.

```rust
// crates/phi-harness/src/error.rs

#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    #[error("harness is busy (phase: {phase:?}), cannot perform structural operation")]
    Busy { phase: HarnessPhase },

    #[error("hook error: {message}")]
    Hook { message: String, #[source] source: anyhow::Error },

    #[error("session error: {0}")]
    Session(#[source] anyhow::Error),

    #[error("compaction error: {0}")]
    Compaction(#[source] anyhow::Error),

    #[error("engine error: {0}")]
    Engine(#[source] anyhow::Error),

    #[error("recovery error: {0}")]
    Recovery { message: String, #[source] source: anyhow::Error },
}
```

### 1.5 PhiHarness struct and public API

`PhiHarness` wraps `Rlm` (or `RlmSession`) and adds the orchestration layer.

```rust
// crates/phi-harness/src/lib.rs

pub struct PhiHarness {
    /// The RLM engine, configured but not running.
    rlm: Rlm,
    /// Session for persistence.
    session: Option<Session>,
    /// Runtime for extension dispatch.
    runtime: Arc<AppRuntime>,
    /// Hook system.
    hooks: PhiHooks,
    /// Current phase (idle/turn/compaction/retry).
    phase: tokio::sync::Mutex<HarnessPhase>,
    /// Harness config (live, not snapshotted).
    config: tokio::sync::RwLock<HarnessConfig>,
    /// Queues (steer, follow-up, next-turn).
    queues: Queues,
    /// Loop detector.
    loop_detector: tokio::sync::Mutex<LoopDetector>,
    /// Observability context.
    observability: ObservabilityContext,
    /// Event channel to TUI/consumers.
    events: UnboundedSender<RlmEvent>,
    /// Cancellation token for the current run.
    cancel: tokio::sync::Mutex<Option<CancellationToken>>,
}

impl PhiHarness {
    /// Start a new turn. Requires Idle phase.
    pub async fn prompt(&self, text: &str) -> Result<HarnessOutcome, HarnessError> {
        // 1. Assert idle, set phase to Turn
        // 2. Create TurnState from harness config
        // 3. Emit before_agent_start hook
        // 4. Call rlm.completion() with the snapshot
        // 5. On completion: flush pending writes, reset phase to Idle
        todo!()
    }

    /// Abort the current run. Allowed during Turn.
    pub async fn abort(&self) -> Result<(), HarnessError> {
        // Cancel token, clear steer+followUp queues
        // Does NOT clear nextTurn queue
        todo!()
    }

    // Structural ops (require Idle)
    pub async fn compact(&self) -> Result<(), HarnessError> { todo!() }
    pub async fn navigate_tree(&self, leaf_id: &str) -> Result<(), HarnessError> { todo!() }

    // Queue ops (allowed during Turn)
    pub async fn steer(&self, text: &str) -> Result<(), HarnessError> { todo!() }
    pub async fn follow_up(&self, text: &str) -> Result<(), HarnessError> { todo!() }
    pub async fn next_turn(&self, text: &str) -> Result<(), HarnessError> { todo!() }

    // Config setters (affect next snapshot, not current run)
    pub async fn set_model(&self, model: Arc<dyn LanguageModel>) { todo!() }
    pub async fn set_thinking(&self, level: Option<ThinkingLevel>) { todo!() }
    pub async fn set_tools(&self, tools: Vec<ToolSpec>, active: Vec<String>) { todo!() }

    // Restore from session log
    pub async fn recover(&self) -> Result<(), HarnessError> { todo!() }
}
```

### 1.6 Migration path

**What changes in `phi-rlm`:**

- `Rlm::completion()` gains an optional `&TurnState` parameter. When provided,
  the loop reads from the snapshot instead of `self.config`. When `None`,
  falls back to current behavior (backwards-compatible).
- `run_loop()` checks for a `TurnState` at each iteration. If present, it uses
  `snapshot.messages`, `snapshot.system_prompt`, etc. instead of
  `self.config` fields.
- `RlmSession::send()` is re-routed through `PhiHarness::prompt()`. The
  session still owns the persistent env and history, but the harness
  manages phase transitions and save points around the call.

**What changes in `phi-app/src/main.rs`:**

- `main.rs` constructs `PhiHarness` wrapping the `Rlm`, instead of calling
  `Rlm::completion()` or `RlmSession::send()` directly.
- The TUI/mux layers receive a `&PhiHandle` (Arc<PhiHarness>) instead of
  a raw `RlmSession`.

**What stays the same:**

- `RlmConfig`, `RlmEvent`, `StopCause`, `LimitKind` -- unchanged.
- `LanguageModel` trait, `ProviderLm`, `LmHandler` -- unchanged.
- `EnvFactory`, `PythonRepl` -- unchanged. The harness creates the env via
  the factory, same as before.
- Session JSONL format -- additive only (new entry types, no changes to
  existing ones).
---

## Phase 2: Lifecycle -- Save Points, Queues, Loop Detection

### 2.1 Save points in `run_loop()`

Currently `run_loop()` in `engine.rs` iterates without reconciling state
between iterations. A save point is inserted after each assistant turn +
REPL execution completes, before the next provider request.

**Where in the loop (current `run_loop` flow):**

1. Send turn message (Turn i/max)
2. Stream root assistant response
3. Extract repl blocks
4. Execute in env
5. Capture stdout/stderr
6. Append formatted result as next user message
**--> SAVE POINT HERE<--**
7. (loop back to step 1 for iteration i+1)

**At the save point, the harness:**

1. **Flushes pending session writes** -- any entries queued during the
   iteration (e.g., from hooks calling `session.append()`) are persisted
   to the JSONL log in deterministic order, after the agent-emitted
   `repl` entry for this iteration.
2. **Drains queues** -- injects any queued steer/follow-up messages into
   the next iteration's user-turn input (see 2.2).
3. **Creates a fresh `TurnState`** if the loop may continue -- re-reads
   harness config (model, thinking, tools, system prompt), so mid-run
   changes take effect on iteration i+1.
4. **Applies the fresh snapshot** -- the next provider request uses the
   new `TurnState`, never the stale one from iteration i.

```rust
// crates/phi-harness/src/save_point.rs

pub struct SavePoint<'a> {
    harness: &'a PhiHarness,
    iteration: usize,
}

impl<'a> SavePoint<'a> {
        // 1. Flush pending session writes
        self.harness.flush_pending_writes().await?;
        let queued = self.harness.queues.drain_at_save_point();
        let snapshot = self.harness.create_turn_state(self.iteration + 1).await?;
        Ok(snapshot.with_queued_messages(queued))
}
```

### 2.2 Queues: steer, follow-up, next-turn

Three queue mechanisms, all stored on `PhiHarness` behind a `Mutex`.

```rust
// crates/phi-harness/src/queue.rs

#[derive(Clone, Debug)]
pub struct QueuedMessage {
    pub text: String,
    pub timestamp: jiff::Timestamp,
    pub source: QueueSource,
}

#[derive(Clone, Debug)]
pub enum QueueSource {
    Steer,
    FollowUp,
    NextTurn,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum QueueMode {
    /// Inject at the next save point (default).
    #[default]
    Interleave,
    /// Accumulate until run ends, then inject as one message.
    Accumulate,
}

pub struct Queues {
    steer: tokio::sync::Mutex<Vec<QueuedMessage>>,
    follow_up: tokio::sync::Mutex<Vec<QueuedMessage>>,
    next_turn: tokio::sync::Mutex<Vec<QueuedMessage>>,
    steer_mode: tokio::sync::RwLock<QueueMode>,
    follow_up_mode: tokio::sync::RwLock<QueueMode>,
}

impl Queues {
    pub async fn enqueue_steer(&self, text: &str) -> String { todo!() }
    pub async fn enqueue_follow_up(&self, text: &str) -> String { todo!() }
    pub async fn enqueue_next_turn(&self, text: &str) -> String { todo!() }

    /// Drain steer + follow-up at save point. Does NOT drain next_turn.
    pub async fn drain_at_save_point(&self) -> Vec<QueuedMessage> { todo!() }

    /// Drain next_turn at the start of the NEXT user-initiated prompt.
    pub async fn drain_next_turn(&self) -> Vec<QueuedMessage> { todo!() }

    /// Clear steer + follow-up (used by abort). Does NOT clear next_turn.
    pub async fn clear_runtime_queues(&self) { todo!() }

    pub async fn set_steer_mode(&self, mode: QueueMode) { todo!() }
    pub async fn set_follow_up_mode(&self, mode: QueueMode) { todo!() }
}
```

**Steer:** injects guidance mid-run without waiting for the turn to end.
At the save point, queued steer messages are prepended to the next
iteration's user-turn message (as a system-style nudge: "Steer: {text}").
The model sees them before its next response.

**Follow-up:** queues the next user message to send after the current run
completes. If the run is still active, the message waits. When the run ends,
the follow-up is the next prompt.

**Next-turn:** inserts messages before the next user-initiated prompt. These
survive abort (unlike steer/follow-up). At `prompt()`, the harness drains
next_turn messages and prepends them to the user's input.

### 2.3 Public API on PhiHarness

```rust
impl PhiHarness {
    /// Inject guidance mid-run. Allowed during Turn.
    pub async fn steer(&self, text: &str) -> Result<String, HarnessError> {
        let id = self.queues.enqueue_steer(text).await;
        Ok(id)
    }

    /// Queue a message for after the current run. Allowed during Turn.
    pub async fn follow_up(&self, text: &str) -> Result<String, HarnessError> {
        let id = self.queues.enqueue_follow_up(text).await;
        Ok(id)
    }

    /// Insert messages before the next user prompt. Survives abort.
    pub async fn next_turn(&self, text: &str) -> Result<String, HarnessError> {
        let id = self.queues.enqueue_next_turn(text).await;
        Ok(id)
    }
}
```

### 2.4 Loop detection

Port Crush's `loop_detection.go` to Rust. Hash tool-call signatures
(tool name + args + result) over a sliding window. Break if any signature
repeats more than N times.

```rust
// crates/phi-harness/src/loop_detect.rs

use sha2::{Sha256, Digest};

const LOOP_WINDOW_SIZE: usize = 10;
const LOOP_MAX_REPEATS: usize = 5;

pub struct LoopDetector {
    /// Ring buffer of recent tool-call signatures.
    window: Vec<String>,
    window_size: usize,
    max_repeats: usize,
}

impl LoopDetector {
    pub fn new() -> Self {
        Self {
            window: Vec::with_capacity(LOOP_WINDOW_SIZE),
            window_size: LOOP_WINDOW_SIZE,
            max_repeats: LOOP_MAX_REPEATS,
        }
    }

    /// Record a tool call + result. Returns true if a loop is detected.
    pub fn observe(&mut self, name: &str, args: &serde_json::Value, result: &str) -> bool {
        let sig = self.signature(name, args, result);
        self.window.push(sig);
        if self.window.len() > self.window_size {
            self.window.remove(0);
        }
        let counts: std::collections::HashMap<&str, usize> =
            self.window.iter().fold(Default::default(), |mut acc, s| {
                *acc.entry(s.as_str()).or_default() += 1;
                acc
            });
        counts.values().any(|&c| c > self.max_repeats)
    }

    fn signature(&self, name: &str, args: &serde_json::Value, result: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(name.as_bytes());
        hasher.update(b"\x00");
        hasher.update(args.to_string().as_bytes());
        hasher.update(b"\x00");
        hasher.update(result.as_bytes());
        hasher.update(b"\x00");
        hex::encode(hasher.finalize())
    }
}
```

**Integration:** The harness wraps the `SubcallHandler` or intercepts
`RlmEvent::ToolStart`/`ToolEnd` pairs. When a loop is detected, the harness
injects a system message telling the model it's repeating and should try a
different approach. If the loop persists, it triggers a new `StopCause`:

```rust
// Add to phi-rlm StopCause enum:
pub enum StopCause {
    // ... existing variants ...
    /// Loop detected: same tool-call signature repeated > max_repeats times.
    LoopDetected,
}
```

### 2.5 Abort and cancellation

`abort()` is allowed during a turn. It:
- Triggers the `CancellationToken` for the current run
- Clears `steer` and `follow_up` queues (they are runtime-only)
- Does NOT clear `next_turn` (survives abort, per Pi's semantics)
- Does NOT discard pending session writes (flushed at next save point or
  in failure cleanup)
- Waits for the run to drain before resetting phase to `Idle`
---

## Phase 3: Hooks Refactor -- Typed Events, Result Chaining, Scopes

### 3.1 The problem with current events

Phi's current event system (`phi-ext/src/lib.rs`) has three issues:

1. **No result chaining**: `AppRuntime::dispatch()` collects returns into
   a `Vec<EventReturn>`, but each handler sees the *original* event, not
   the output of prior handlers. Handler B can't see handler A's modification.
2. **No observe vs participate**: all matching handlers run independently.
   There's no read-only observation path that sees all events without
   participating in semantics.
3. **No source metadata**: no way to know which extension produced a result,
   an error, or a resource. No cleanup lifecycle per hook.

### 3.2 New hook event model

Pi uses TypeScript phantom types (`ResultOf<E>`) to let each event define
its own result type. In Rust, the cleanest equivalent is a trait with an
associated type for the result.

```rust
// crates/phi-harness/src/hooks.rs

pub trait HookEvent: Send + Sync {
    type Result: Send + Sync + Default;
}

/// A handler that participates in event semantics and may return a result.
pub type HookHandler<E> = Arc<dyn Fn(&E) -> Result<Option<E::Result>, HarnessError> + Send + Sync>;

/// An observer that sees all events read-only. Return ignored.
pub type HookObserver = Arc<dyn Fn(&dyn std::any::Any) -> Result<(), HarnessError> + Send + Sync>;
```

### 3.3 Concrete event types

Replace the current `EventKind`/`EventPayload`/`EventReturn` enums with
typed structs that implement `HookEvent`.

```rust

// -- Context transform: each handler sees current messages --
pub struct ContextEvent {
    pub messages: Vec<ChatMessage>,
}
impl HookEvent for ContextEvent {
    type Result = ContextResult;
}
pub struct ContextResult {
    pub messages: Option<Vec<ChatMessage>>,
}

// -- Tool call: early exit on block --
pub struct ToolCallEvent {
    pub tool_name: String,
    pub input: serde_json::Value,
}
impl HookEvent for ToolCallEvent {
    type Result = ToolCallResult;
}
pub struct ToolCallResult {
    pub block: bool,
    pub reason: Option<String>,
}

// -- Tool result: patch accumulation --
pub struct ToolResultEvent {
    pub tool_name: String,
    pub content: String,
    pub is_error: bool,
}
impl HookEvent for ToolResultEvent {
    type Result = ToolResultPatch;
}
pub struct ToolResultPatch {
    pub content: Option<String>,
    pub is_error: Option<bool>,
}

// -- Before agent start: collect injected messages, chain system prompt --
pub struct BeforeAgentStartEvent {
    pub system_prompt: String,
    pub prompt: String,
}
impl HookEvent for BeforeAgentStartEvent {
    type Result = BeforeAgentStartResult;
}
pub struct BeforeAgentStartResult {
    pub messages: Vec<ChatMessage>,
    pub system_prompt: Option<String>,
}

// -- Observation-only events (result = ()) --
pub struct MessageEndEvent {
    pub message: AssistantMessage,
}
impl HookEvent for MessageEndEvent {
    type Result = ();
}

pub struct RunStartEvent { pub session_id: String }
impl HookEvent for RunStartEvent { type Result = (); }

pub struct RunEndEvent { pub stop_cause: StopCause }
impl HookEvent for RunEndEvent { type Result = (); }
```

### 3.4 PhiHooks struct

```rust
pub struct PhiHooks {
    /// Read-only observers: see all events, return ignored.
    observers: Vec<(SourceInfo, HookObserver)>,
    /// Type-erased handler map: event type name -> Vec<(SourceInfo, handler)>.
    handlers: RwLock<BTreeMap<String, Vec<HandlerEntry>>>,
    /// Cleanup functions registered by hooks.
    cleanups: Mutex<Vec<CleanupEntry>>,
}

struct HandlerEntry {
    source: SourceInfo,
    handler: Arc<dyn Any + Send + Sync>,  // type-erased HookHandler<E>
}

#[derive(Clone, Debug, Default)]
pub struct SourceInfo {
    pub extension_id: String,
    pub label: Option<String>,
}

struct CleanupEntry {
    source: SourceInfo,
    cleanup: Box<dyn FnOnce() -> Result<(), HarnessError> + Send + Sync>,
}

impl PhiHooks {
    /// Register a read-only observer. Returns a cleanup handle.
    pub fn observe(&self, observer: HookObserver) -> impl FnOnce() {
        todo!()
    }

    /// Register a typed handler for a specific event. Returns a cleanup handle.
    pub fn on<E: HookEvent + 'static>(
        &self,
        handler: HookHandler<E>,
    ) -> impl FnOnce() {
        todo!()
    }

    /// Emit an event. Runs observers, then reduces handlers with chaining.
    pub async fn emit<E: HookEvent + 'static>(&self, event: E) -> Result<Option<E::Result>, HarnessError> {
        todo!()
    }

    /// Register a cleanup function.
    pub fn add_cleanup<F>(&self, source: SourceInfo, cleanup: F)
    where
        F: FnOnce() -> Result<(), HarnessError> + Send + Sync + 'static,
    {
        todo!()
    }

    /// Create a scoped source for registering hooks from an extension.
    pub fn create_scope(&self, source: SourceInfo) -> HookScope {
        HookScope { hooks: self, source }
    }

    /// Remove all handlers and run cleanups.
    pub async fn clear(&self) -> Result<(), HarnessError> {
        todo!()
    }
}

pub struct HookScope<'a> {
    hooks: &'a PhiHooks,
    source: SourceInfo,
}
```

### 3.5 Result chaining reducers

Each event type has a specific chaining strategy. The `emit()` method
dispatches to the right reducer based on the event type.

**Context transform (sequential, each sees current):**
```rust
// Handlers run in order. Each sees the current messages.
let mut current = event;
let mut modified = false;
for (source, handler) in handlers {
    let handler = handler.downcast_ref::<HookHandler<ContextEvent>>().unwrap();
    let result = handler(&current)?;
    if let Some(r) = result {
        if let Some(msgs) = r.messages {
            current.messages = msgs;
            modified = true;
        }
    }
}
Ok(if modified { Some(ContextResult { messages: Some(current.messages) }) } else { None })
```

**Tool call (sequential, early exit on block):**
```rust
for (source, handler) in handlers {
    let result = handler(&event)?;
    if let Some(r) = result {
        if r.block {
            return Ok(Some(r));  // early exit
        }
    }
}
Ok(None)
```

**Tool result (patch accumulation, each sees current):**
```rust
let mut current = event;
let mut modified = false;
for (source, handler) in handlers {
    let result = handler(&current)?;
    if let Some(patch) = result {
        if let Some(content) = patch.content { current.content = content; modified = true; }
        if let Some(is_err) = patch.is_error { current.is_error = is_err; modified = true; }
    }
}
Ok(if modified { Some(ToolResultPatch { content: Some(current.content), is_error: Some(current.is_error) }) } else { None })
```

### 3.6 Migration path

**phi-ext:**
- `EventKind`, `EventPayload`, `EventReturn` enums are deprecated.
- New typed event structs (`ContextEvent`, `ToolCallEvent`, etc.) live in
  `phi-harness` and re-exported via `phi-ext`.
- `EventHandler` type alias changes to use the new `HookHandler<E>` pattern.
- `ExtensionHost::subscribe()` evolves to accept typed handlers via
  `PhiHooks::on::<E>()`.

**phi-runtime:**
- `AppRuntime::dispatch()` is replaced by `AppRuntime::hooks()` which returns
  `&PhiHooks`. The harness calls `hooks.emit(event)` instead of dispatching.
- `filter_system_prompt()` and `context_injections()` become thin wrappers
  around `hooks.emit(BeforeAgentStartEvent { ... })`.
- The `extension_prelude_stubs()` and tool invocation paths are unchanged
  (they're registries, not hooks, per Pi's design).

**phi-builtins / phi-lua:**
- Builtin extensions register handlers via `hooks.on::<ContextEvent>(...)`
  instead of `host.subscribe(EventHandlerRegistration { ... })`.
- Lua extensions get a shim layer: `hooks.on("context", function(ctx, ev) ... end)`
  where the Lua handler is wrapped in a typed `HookHandler`.
---

## Phase 4: Durability + Observability

### 4.1 New durable entry types

Extend the existing JSONL session schema (additive only -- existing entry
types like `message`, `repl`, `exchange_start/end`, `leaf` are unchanged).
New types for operation-level recovery:

```rust
// In phi-session/src/session.rs, new serde structs:

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type")]
pub enum DurableEntry {
    /// A harness operation (prompt, compaction, etc.) started.
    OperationStart {
        id: String,
        timestamp: String,
        operation: OperationKind,  // Prompt, Compaction, TreeNavigation
        session_id: String,
    },
    /// Operation completed successfully.
    OperationFinish {
        id: String,
        timestamp: String,
        operation_id: String,
        stop_cause: Option<String>,
    },
    /// Operation was interrupted (crash recovery signature).
    OperationInterrupted {
        id: String,
        timestamp: String,
        operation_id: String,
        reason: String,  // "crash" or "abort"
    },
    /// A turn (one RLM iteration within a run) started.
    TurnStart {
        id: String,
        timestamp: String,
        operation_id: String,
        iteration: usize,
        consumed_queue_ids: Vec<String>,  // steer/follow-up IDs consumed
    },
    /// Turn completed.
    TurnFinish {
        id: String,
        timestamp: String,
        turn_id: String,
        duration_secs: f64,
    },
    /// A prelude tool call started (for safe recovery).
    ToolCallStart {
        id: String,
        timestamp: String,
        turn_id: String,
        tool_name: String,
        args_preview: String,  // truncated
    },
    /// Tool call completed.
    ToolCallFinish {
        id: String,
        timestamp: String,
        tool_call_id: String,
        result_preview: String,  // truncated
        error: Option<String>,
    },
    /// Queue message enqueued (steer/follow-up/next_turn).
    QueueEnqueued {
        id: String,
        timestamp: String,
        queue: String,  // "steer" | "follow_up" | "next_turn"
        text: String,
    },
    /// Queue message consumed at a save point.
    QueueConsumed {
        id: String,
        timestamp: String,
        queue_message_id: String,
        turn_id: String,
    },
    /// Pending session write enqueued during active operation.
    PendingWriteEnqueued {
        id: String,
        timestamp: String,
        entry_type: String,  // type tag of the deferred entry
        entry_data: serde_json::Value,  // serialized entry
    },
    /// Pending write applied to the session log.
    PendingWriteApplied {
        id: String,
        timestamp: String,
        pending_write_id: String,
        target_entry_id: String,  // the actual entry id in the log
    },
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum OperationKind {
    Prompt,
    Compaction,
    TreeNavigation,
}
```

The tolerant reader already skips unknown types, so old binaries read new
files. New entry types are additive -- no migration needed.

### 4.2 Recovery logic

`PhiHarness::recover()` reduces the session log to reconstruct state.

```rust
impl PhiHarness {
    /// Reconstruct harness state from the session log.
    /// Called on startup / session resume.
    pub async fn recover(&self) -> Result<RecoveryReport, HarnessError> {
        let entries = self.session.read_all_entries().await?;
        let mut reducer = RecoveryReducer::new();
        for entry in entries {
            reducer.process(entry)?;
        }
        let report = reducer.finalize();

        // Validate runtime dependencies
        self.validate_runtime_deps(&report)?;

        // Reconcile unfinished operations (conservative policy)
        self.reconcile_unfinished(&report).await?;

        // Restore queues, pending writes, phase
        self.restore_state(&report).await?;

        Ok(report)
    }
}

pub struct RecoveryReport {
    pub current_leaf: Option<String>,
    pub restored_queue_messages: Vec<QueuedMessage>,
    pub pending_writes: Vec<PendingWrite>,
    pub unfinished_operation: Option<UnfinishedOperation>,
    pub model_changes: Vec<ModelChange>,
    pub active_tools: Option<Vec<String>>,
}

pub struct UnfinishedOperation {
    pub operation_id: String,
    pub kind: OperationKind,
    pub last_completed_turn: Option<usize>,
}
```

**Conservative recovery policy (default: `mark_interrupted`):**

| Scenario | Recovery action |
|---|---|
| Unfinished agent turn | Mark `OperationInterrupted`, preserve queues/pending writes, return to Idle |
| Unfinished provider request | Mark interrupted; do NOT retry automatically |
| Unfinished tool call | Append interrupted/error tool result; retry only if tool declares retry-safe |
| Unfinished compaction | Rerun if no compaction entry exists |
| Unfinished tree navigation | Apply missing leaf entry if safe |

**Recovery invariant:** consumed queue IDs must be recorded in `TurnStart`
before they are considered consumed. Crash after drain but before
`TurnStart` = risk of loss/duplication, mitigated by the durable
`QueueConsumed` entry.

### 4.3 Observability

Port Pi's observability design. Runtime-agnostic: trace/span tree with
stable event names, safe payloads, and a subscriber API.

```rust
// crates/phi-harness/src/observe.rs

#[derive(Clone, Debug, Default)]
pub struct ObservabilityContext {
    pub trace_id: Option<String>,
    pub current_span_id: Option<String>,
    pub user_context: serde_json::Value,
}

#[derive(Clone, Debug, Serialize)]
pub struct ObservabilityEvent {
    pub r#type: ObservabilityEventType,  // Start, End, Error, Event
    pub name: String,                   // e.g. "phi.agent.prompt"
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub timestamp: jiff::Timestamp,
    pub duration_ms: Option<f64>,
    pub context: serde_json::Value,
    pub payload: serde_json::Value,
    pub error: Option<ErrorInfo>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ErrorInfo {
    pub name: String,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Serialize)]
[serde(rename_all = "snake_case")]
pub enum ObservabilityEventType {
    Start,
    End,
    Error,
    Event,
}
```

**`trace_operation()` in Rust:**

Rust does not have `AsyncLocalStorage` built in, but `tokio::task_local!`
provides the equivalent for task-scoped context propagation. The harness
threads an `ObservabilityContext` explicitly for deterministic control,
with `task_local!` as the propagation mechanism for deep calls.

```rust
tokio::task_local! {
    static OBS_CONTEXT: ObservabilityContext;
}

pub async fn trace_operation<T, F, Fut>(
    name: &str,
    payload: serde_json::Value,
    f: F,
) -> Result<T, HarnessError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T, HarnessError>>,
{
    let parent = OBS_CONTEXT.try_get().map(|c| c.clone()).unwrap_or_default();
    let trace_id = parent.trace_id.clone().unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
    let span_id = uuid::Uuid::now_v7().to_string();
    let parent_span_id = parent.current_span_id.clone();

    let child = ObservabilityContext {
        trace_id: Some(trace_id.clone()),
        current_span_id: Some(span_id.clone()),
        user_context: parent.user_context.clone(),
    };

    emit_observability(ObservabilityEvent {
        r#type: ObservabilityEventType::Start,
        name: name.to_string(),
        trace_id: trace_id.clone(),
        span_id: span_id.clone(),
        parent_span_id: parent_span_id.clone(),
        timestamp: jiff::Timestamp::now(),
        duration_ms: None,
        context: parent.user_context.clone(),
        payload,
        error: None,
    }).await;

    let result = OBS_CONTEXT.scope(child, f()).await;

    match &result {
        Ok(_) => {
            emit_observability(ObservabilityEvent {
                r#type: ObservabilityEventType::End,
                name: name.to_string(),
                trace_id,
                span_id,
                parent_span_id,
                timestamp: jiff::Timestamp::now(),
                duration_ms: None, // filled by caller or timer
                context: parent.user_context,
                payload: serde_json::Value::Null,
                error: None,
            }).await;
        }
        Err(e) => {
            emit_observability(ObservabilityEvent {
                r#type: ObservabilityEventType::Error,
                name: name.to_string(),
                trace_id,
                span_id,
                parent_span_id,
                timestamp: jiff::Timestamp::now(),
                duration_ms: None,
                context: parent.user_context,
                payload: serde_json::Value::Null,
                error: Some(ErrorInfo { name: "HarnessError".into(), message: e.to_string() }),
            }).await;
        }
    }

    result
}
```

### 4.4 Stable event names and instrumentation points

| Event name | Where to instrument |
|---|---|
| `phi.agent.prompt` | `PhiHarness::prompt()` |
| `phi.agent.turn` | `run_loop()` per iteration |
| `phi.agent.tool_call` | `EngineSubcalls::handle()` for ExtTool |
| `phi.agent.subcall` | `EngineSubcalls::query()` / `recurse()` |
| `phi.agent.compaction` | `PhiHarness::compact()` |
| `phi.agent.tree_navigation` | `PhiHarness::navigate_tree()` |
| `phi.ai.provider.request` | `ProviderLm` / `LmHandler` |
| `phi.session.append_entry` | `Session::append()` |

### 4.5 Safe payload redaction

**Safe by default:** provider, model, api identifier, session id, entry
type, tool name, status code, stop reason, token counts, costs, durations.

**Unsafe by default (never included in payloads):** prompts, completions,
tool args, tool results, shell output, file contents, provider request
payloads, provider response bodies, API keys, headers.

Content capture can be opt-in later with explicit redaction hooks.

### 4.6 Subscriber API

```rust
pub type ObservabilitySubscriber = Arc<dyn Fn(&ObservabilityEvent) + Send + Sync>;

static SUBSCRIBERS: OnceLock<Vec<ObservabilitySubscriber>> = OnceLock::new();

pub fn subscribe_observability(subscriber: ObservabilitySubscriber) -> impl FnOnce() {
    // Add to SUBSCRIBERS, return cleanup closure
    todo!()
}

async fn emit_observability(event: ObservabilityEvent) {
    if let Some(subs) = SUBSCRIBERS.get() {
        for sub in subs {
            // Subscriber errors are swallowed, never affect execution
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sub(&event)));
        }
    }
}
```
---

## Implementation Timeline

Six phases, ordered from foundational to advanced. Each phase is independently
shippable and testable.

### Phase 1: Foundation (1-2 weeks)

**Goal:** Crate skeleton, phase enum, TurnState, HarnessError, PhiHarness wrapper.

| Step | Deliverable |
|---|---|
| 1.1 | Create `phi-harness` crate with Cargo.toml, add to workspace |
| 1.2 | `HarnessPhase` enum with idle/turn/compaction/retry |
| 1.3 | `TurnState` struct |
| 1.4 | `HarnessError` enum with thiserror |
| 1.5 | `PhiHarness` struct wrapping `Rlm`, with `prompt()` and `abort()` |
| 1.6 | Modify `Rlm::completion()` to accept optional `&TurnState` |
| 1.7 | Wire `phi-app/main.rs` to construct `PhiHarness` instead of raw `Rlm` |

**Exit criteria:** `phi-app` runs through `PhiHarness::prompt()` with the same
behavior as before (no regressions). Phase transitions work: calling
`prompt()` while busy returns `HarnessError::Busy`.

### Phase 2: Lifecycle (2 weeks)

**Goal:** Save points, queues, loop detection.

| Step | Deliverable |
|---|---|
| 2.1 | Save point logic in `run_loop()` — flush pending writes, create fresh snapshot |
| 2.2 | `Queues` struct with steer/follow-up/next-turn |
| 2.3 | Queue drain at save points |
| 2.4 | `steer()`, `follow_up()`, `next_turn()` public API |
| 2.5 | `LoopDetector` with sliding-window signature hashing |
| 2.6 | `StopCause::LoopDetected` variant |
| 2.7 | Abort clears steer+follow-up but not next_turn |

**Exit criteria:** User can `steer()` mid-run and the model sees guidance on
the next iteration. `LoopDetector` breaks a repeating tool-call loop.
Follow-up messages queue across runs.

### Phase 3: Hooks Refactor (2-3 weeks)

**Goal:** Typed hooks with observe/on/emit, result chaining, source metadata.

| Step | Deliverable |
|---|---|
| 3.1 | `HookEvent` trait with associated `Result` type |
| 3.2 | Concrete event structs (`ContextEvent`, `ToolCallEvent`, etc.) |
| 3.3 | `PhiHooks` struct with `observe()`, `on()`, `emit()`, `add_cleanup()` |
| 3.4 | Result chaining reducers (context transform, tool_call block, tool_result patch) |
| 3.5 | `SourceInfo` and `HookScope` for extension provenance |
| 3.6 | Migrate `phi-runtime::AppRuntime::dispatch()` to delegate to `PhiHooks::emit()` |
| 3.7 | Migrate `phi-builtins` to use new typed hooks |
| 3.8 | Lua shim layer in `phi-lua` for `hooks.on("event", fn)` |

**Exit criteria:** Handler B sees handler A's modifications to context messages.
`observe()` handlers see all events without participating. Source metadata
identifies which extension produced a result. Builtins work through the new
system.

### Phase 4: Durability (1-2 weeks)

**Goal:** Durable operation/turn/tool-call entries, recovery via log reduction.

| Step | Deliverable |
|---|---|
| 4.1 | New durable entry types in `phi-session` serde structs |
| 4.2 | Write `OperationStart/Finish/Interrupted` at harness boundaries |
| 4.3 | Write `TurnStart/TurnFinish` with consumed queue IDs |
| 4.4 | Write `QueueEnqueued/Consumed` for steer/follow-up/next_turn |
| 4.5 | `PendingWriteEnqueued/Applied` for deferred session writes |
| 4.6 | `RecoveryReducer` — reduces log to `RecoveryReport` |
| 4.7 | `PhiHarness::recover()` — validate deps, reconcile unfinished ops |
| 4.8 | Conservative interruption policy (mark_interrupted) |

**Exit criteria:** Kill phi mid-run, restart, and `recover()` reconstructs
queue state, pending writes, and marks the interrupted operation. No
duplicate tool execution on recovery.

### Phase 5: Observability (1-2 weeks)

**Goal:** Trace/span tree, stable event names, subscriber API.

| Step | Deliverable |
|---|---|
| 5.1 | `ObservabilityContext` and `ObservabilityEvent` structs |
| 5.2 | `trace_operation()` with `tokio::task_local!` |
| 5.3 | Instrument `PhiHarness::prompt()`, `run_loop()`, `EngineSubcalls` |
| 5.4 | Instrument `ProviderLm` / `LmHandler` for `phi.ai.provider.request` |
| 5.5 | Safe payload redaction (safe fields only by default) |
| 5.6 | `subscribe_observability()` API |
| 5.7 | Bridge to `RlmEvent` (TUI consumes observability events) |

**Exit criteria:** A subscriber receives a trace tree for a full `prompt()`
call: prompt -> turn -> provider.request -> tool_call -> subcall, with
span IDs and parent relationships. No unsafe content in payloads.

### Phase 6: Hardening (1-2 weeks)

| Step | Deliverable |
|---|---|
| 6.1 | `HarnessError` adoption across all public APIs |
| 6.2 | Reentrancy tests: hooks call `steer()` during emit |
| 6.3 | Phase cleanup tests: success, provider error, hook error, abort, compaction |
| 6.4 | Queue ordering tests: deterministic drain at save points |
| 6.5 | Recovery scenario tests: crash at each durable entry point |
| 6.6 | Observability subscriber isolation tests (errors don't affect execution) |
| 6.7 | Loop detection edge cases (empty results, identical args different result) |

---

## Dependency Graph of Implementation

```
Phase 1 (Foundation)
    |
    +-- Phase 2 (Lifecycle: save points, queues, loop detection)
    |       (needs TurnState from Phase 1)
    |
    +-- Phase 3 (Hooks Refactor)
    |       (needs HarnessError from Phase 1)
    |
    +-- Phase 4 (Durability)
    |       (needs phases + queues from Phases 1-2)
    |
    +-- Phase 5 (Observability)
    |       (needs trace points in harness from Phase 1)
    |
    +-- Phase 6 (Hardening)
            (needs all prior phases)
```

Phases 2, 3, 4, and 5 can proceed in parallel after Phase 1, since they
each only depend on the foundation (TurnState, HarnessError, PhiHarness).
Phase 4 (Durability) does benefit from Phase 2's queues being defined but can
define the entry types speculatively.

---

## Testing Strategy

### Faux provider

Port Pi's `registerFauxProvider` pattern: a test-only `LanguageModel` impl
that returns scripted completions without real API calls. This enables
deterministic harness tests.

```rust
// In test module or xtask:
pub struct FauxLm {
    responses: Vec<String>,  // scripted responses
    call_count: AtomicUsize,
}

#[async_trait]
impl LanguageModel for FauxLm {
    async fn complete(&self, _prompt: Prompt) -> Completion {
        let idx = self.call_count.fetch_add(1, Ordering::SeqCst);
        let text = self.responses.get(idx).cloned().unwrap_or_default();
        Completion { text, usage: Usage::default(), .. }
    }
}
```

### Test areas

| Area | What to test |
|---|---|
| Phase lifecycle | Busy-rejection, phase transitions, cleanup on error |
| Save points | Mid-run config changes take effect on next iteration |
| Queues | Steer injection, follow-up after run, next_turn survives abort |
| Loop detection | Repeating tool calls detected, different results not flagged |
| Hooks chaining | Handler B sees A's output, early exit on block, patch accumulation |
| Hooks observers | See all events, return ignored, no semantic effect |
| Durability | Crash at each entry point, recovery reconstructs correct state |
| Observability | Span tree structure, safe payloads, subscriber isolation |
| Reentrancy | Hooks calling harness APIs (steer, set_model) during emit |

---

## What This Unlocks

Once complete, the harness layer enables:

- **Live steering**: users (and extensions) can guide the agent mid-run
- **Interruptible runs**: abort cleanly, with queued messages preserved
- **Extension participation**: hooks can transform context, block tools,
  patch results -- not just observe
- **Crash recovery**: kill phi mid-run, restart, and resume with
  interrupted-operation marking and queue restoration
- **Observability**: OTel/Sentry/logs/metrics via a stable event contract
- **Loop prevention**: the agent breaks out of repetitive tool-call cycles
- **Deterministic config changes**: model/thinking/tool swaps take effect
  at save points, not mid-request
- **Typed errors**: all public failures have structured codes, not
  ad-hoc strings

The "disconnected" feeling resolves because the harness becomes the shared
lifecycle contract that ties the engine, session, extensions, and TUI
together. Every component talks to the harness, not to each other.