# Plan: Lua Extension System

Goal: pi-parity extension surface, with the intent to add ori-mnemos as a Lua
extension. Lua chosen as the scripting runtime (mlua 0.11/lua54 already in workspace).

## Architecture Overview

```
.phi/extensions/*.lua          ~/.config/phi/extensions/*.lua
        │                               │
        └──────────── phi-lua ──────────┘
                          │  discover + load, expose `phi` global
                          ▼
                    phi-runtime (AppRuntime)
                    ┌─ tool registry ────── → prelude.py stubs → REPL
                    ├─ command registry ──── → TUI slash dispatch
                    └─ event handlers ────── → RLM loop hooks
                                             → TUI input hooks
```

## 1. phi-ext — Type System Overhaul

### 1a. EventKind — expand from 7 to 14 kinds

```rust
pub enum EventKind {
    // existing (keep)
    RuntimeStart, SessionStart, TuiReady,
    // agent loop
    AgentStart, AgentEnd,
    // per-turn
    TurnStart, TurnEnd,
    // context injection (return-capable)
    BeforeAgentStart,   // fires once before run: can append/prepend system prompt
    Context,            // fires before each LM call: can prepend messages
    // tool lifecycle (return-capable)
    ToolCall,           // fires before a prelude tool runs: can block
    ToolResult,         // fires after a prelude tool returns: can override result
    // input (return-capable)
    Input,              // fires when user submits: can transform or swallow
    // teardown
    SessionShutdown,
    // remove: BeforeModel, AfterModel, BeforeTool, AfterTool (subsumed above)
}
```

### 1b. LifecycleEvent — structured payloads (replace `summary: String`)

```rust
pub struct LifecycleEvent {
    pub kind: EventKind,
    pub payload: EventPayload,
}

pub enum EventPayload {
    Empty,
    SessionStart   { cwd: String, session_id: String },
    BeforeAgentStart { prompt: String, system_prompt: String },
    Context        { turn: u32 },
    TurnStart      { turn: u32 },
    TurnEnd        { turn: u32, had_code: bool },
    AgentEnd       { stop_cause: String },
    ToolCall       { name: String, args: serde_json::Value },
    ToolResult     { name: String, result: String, error: Option<String> },
    Input          { text: String },
}
```

### 1c. EventReturn — mutation returns for hooks

```rust
pub enum EventReturn {
    SystemPromptMod { append: Option<String>, prepend: Option<String> },
    MessageInjection { prepend: Vec<(String, String)> },  // (role, content)
    ToolBlock        { reason: String },
    ToolResultOverride { content: String },
    InputAction      { action: InputActionKind, text: Option<String> },
}

pub enum InputActionKind { Continue, Transform, Handled }
```

### 1d. Handler types — async, return EventReturn

```rust
use futures::future::BoxFuture;

pub type EventHandler = Arc<
    dyn Fn(LifecycleEvent) -> BoxFuture<'static, Result<Option<EventReturn>>>
    + Send + Sync
>;
pub type ToolHandler = Arc<dyn Fn(ToolInvocation) -> Result<ToolOutput> + Send + Sync>;
pub type CommandHandler = Arc<dyn Fn(CommandInvocation) -> Result<CommandOutput> + Send + Sync>;
```

Remove `EventContext` (fields merged into `EventPayload`).

### 1e. ToolSpec — add `params` for Python stub generation

```rust
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub params: Vec<ToolParam>,     // new: ordered param specs
    pub handler: ToolHandler,
}

pub struct ToolParam {
    pub name: String,
    pub ty: String,         // Python type hint: "str", "int", "dict", etc.
    pub default: Option<String>,  // Python default expr or None
}
```

## 2. phi-lua — New Crate

New crate: `crates/phi-lua/`. Depends on: `phi-ext`, `mlua`, `anyhow`, `serde_json`, `tokio`.

### 2a. LuaExtension — wraps a single .lua file

Implements `phi_ext::Extension`. On `register()`:
- Creates a `mlua::Lua` instance
- Sets `phi` global to a Lua table with these methods:
  - `phi.on(event_name, handler_fn)` — stores the Lua function keyed by event name
  - `phi.register_tool(spec_table)` — calls `host.register_tool()` with a Rust handler that calls the Lua `execute` fn
  - `phi.register_command(name, opts_table)` — calls `host.register_command()`
  - `phi.notify(msg)` — enqueues a notification (stored in a shared queue, drained by TUI)
  - `phi.set_status(key, text)` — sets a status line item
  - `phi.clear_status(key)` — clears a status line item
- Executes the Lua file (chunk runs, registrations happen as side effects)
- Wraps stored Lua functions as `EventHandler` closures that call the Lua fn async

The Lua VM must be kept alive for the lifetime of the extension (Arc<Mutex<Lua>>).

Each Lua EventHandler calls the stored Lua function with a table matching the payload
variant, awaits the result, and maps the returned table to `Option<EventReturn>`.

### 2b. Lua event payload mapping

Events are passed to Lua as tables. Return values are parsed from Lua tables:

| Event | Lua table fields | Return fields |
|-------|-----------------|---------------|
| `before_agent_start` | `{prompt, system_prompt}` | `{system_prompt_append?, system_prompt_prepend?}` |
| `context` | `{turn}` | `{prepend_messages?: [{role,content}]}` |
| `tool_call` | `{name, args}` | `{block?: bool, reason?: str}` |
| `tool_result` | `{name, result, error?}` | `{result?: str}` |
| `input` | `{text}` | `{action: "continue"\|"transform"\|"handled", text?: str}` |
| `session_start` | `{cwd, session_id}` | _(ignored)_ |
| `turn_start` | `{turn}` | _(ignored)_ |
| `turn_end` | `{turn, had_code}` | _(ignored)_ |
| `agent_start` | `{}` | _(ignored)_ |
| `agent_end` | `{stop_cause}` | _(ignored)_ |
| `session_shutdown` | `{}` | _(ignored)_ |
| `tui_ready` | `{}` | _(ignored)_ |

### 2c. Lua tool stub generation

`LuaExtension` exposes a `tool_prelude_stubs() -> String` method that returns Python
source for all registered tools. Each tool becomes:

```python
def my_tool(param_a: str, param_b: int = 0):
    return __phi_ext_tool__("my_tool", {"param_a": param_a, "param_b": param_b})
```

This is concatenated into `RlmConfig::setup_code` at session build time.

### 2d. Extension discovery

```rust
pub fn discover_extensions(cwd: &Path) -> Vec<PathBuf>
```

Scans (in order, later entries override earlier):
1. `~/.config/phi/extensions/*.lua`
2. `{cwd}/.phi/extensions/*.lua`

Returns sorted list of `.lua` paths. Logs skipped files on error.

```rust
pub fn load_extensions(paths: &[PathBuf]) -> Vec<Box<dyn Extension>>
```

Wraps each path in a `LuaExtension`. Load errors are logged and skipped (non-fatal).

## 3. phi-runtime — Async Dispatch + Context Injection

### 3a. Make AppRuntime Arc-friendly

```rust
pub struct AppRuntime { ... }  // unchanged fields, but expose via Arc<AppRuntime>
```

Add `Arc<Mutex<Vec<String>>>` notification queue (written by `phi.notify()` in Lua,
drained by the TUI into scrollback).

Add `Arc<Mutex<IndexMap<String, String>>>` status map (written by `phi.set_status()`).

### 3b. Async dispatch with return collection

```rust
impl AppRuntime {
    pub async fn dispatch(&self, event: LifecycleEvent) -> Result<Vec<EventReturn>> { ... }
    // fires all matching handlers, collects non-None returns
}
```

Convenience helpers:
```rust
pub async fn filter_system_prompt(&self, prompt: String, system_prompt: String) -> Result<String>
// fires BeforeAgentStart, applies all SystemPromptMod returns in registration order

pub async fn filter_messages(&self, turn: u32) -> Result<Vec<(String, String)>>
// fires Context, collects all MessageInjection prepends in registration order

pub async fn check_tool_call(&self, name: &str, args: &Value) -> Result<Option<String>>
// fires ToolCall, returns Some(reason) if any handler blocks it

pub async fn filter_tool_result(&self, name: &str, result: &str, error: Option<&str>) -> Result<String>
// fires ToolResult, applies first ToolResultOverride if any

pub async fn check_input(&self, text: &str) -> Result<InputCheck>
// fires Input, returns transform/swallow/pass-through
```

### 3c. Tool prelude contribution

```rust
impl AppRuntime {
    pub fn extension_prelude_stubs(&self) -> String
    // returns concatenated Python stubs for all registered extension tools
}
```

### 3d. Extension command lookup

```rust
impl AppRuntime {
    pub fn extension_command_names(&self) -> Vec<&str>
    pub fn extension_command_descs(&self) -> Vec<(&str, &str)>
    pub async fn invoke_command(&self, name: &str, args: &str) -> Result<Option<CommandOutput>>
}
```

## 4. phi-builtins — Prelude Extension Point

### 4a. prelude.py — add `__phi_ext_tool__`

Add to `prelude.py` (before the existing tool definitions):

```python
def __phi_ext_tool__(name, args):
    """Route call to a Rust-registered extension tool via the subcall protocol."""
    result = _subcall("ext_tool", name=name, args=args)
    return result
```

The `_subcall("ext_tool", ...)` is the existing subcall mechanism — add `ext_tool` as
a new subcall kind in `phi-repl`.

### 4b. coding_tools_section — accept extension tools param

```rust
pub fn coding_tools_section(ext_tools: &[ToolSpec]) -> String
```

If `ext_tools` is non-empty, appends a section:
```
Extension tools (registered by loaded extensions):
- my_tool(param_a: str, param_b: int = 0) -> str — description
```

## 5. phi-repl — ExtTool Subcall

### 5a. SubcallRequest — add ExtTool variant

In `phi_repl::SubcallHandler`:

```rust
pub enum SubcallRequest {
    Llm  { prompt: String, model: Option<String> },
    Rlm  { prompt: String, model: Option<String> },
    HostFn { name: String, args: Value, kwargs: Value },
    ExtTool { name: String, args: Value },   // new
}
```

### 5b. driver.py — add ext_tool subcall kind

In `_subcall()`, handle `kind == "ext_tool"`:

```python
elif kind == "ext_tool":
    result = _do_subcall(id, "ext_tool", name=kwargs.get("name"), args=kwargs.get("args", {}))
    return result
```

### 5c. EngineSubcalls — route ExtTool to AppRuntime

In `phi-rlm`'s subcall handler, `SubcallRequest::ExtTool { name, args }` is routed to
`AppRuntime::invoke_tool(name, args)` and the result returned as `SubcallResponse::Text`.

## 6. phi-rlm — Fire New Events

`Rlm` and `RlmSession` gain an `Option<Arc<AppRuntime>>` field, set at build time.

New event firings in `engine.rs`:

| Location | Event fired |
|----------|-------------|
| `run_loop()` start | `AgentStart` |
| `run_loop()` end | `AgentEnd { stop_cause }` |
| Each iteration start (after emit IterationStart) | `TurnStart { turn }` |
| Each iteration end | `TurnEnd { turn, had_code }` |
| Before first LM call in run | `filter_system_prompt()` → augment `RlmConfig::system` |
| Before each `complete_root()` call | `filter_messages()` → prepend synthetic messages |
| After ToolEvent::Start received | `check_tool_call()` → early-exit with error if blocked |
| After ToolEvent::End received | `filter_tool_result()` → replace result |

## 7. phi-session — Wire AppRuntime into TUI

### 7a. TuiSetup — add AppRuntime

```rust
pub struct TuiSetup {
    pub rlm: Rlm,
    pub payload: ContextPayload,
    pub events: UnboundedReceiver<RlmEvent>,
    pub missing_credentials: Vec<String>,
    pub ui: UiSetup,
    pub runtime: Option<Arc<AppRuntime>>,   // new
}
```

### 7b. Slash command dispatch — check extension commands

In `Ui::submit()`, after the built-in `match`, before sending as prompt:

```rust
if input.starts_with('/') {
    let (name, args) = split_command(input);
    if let Some(output) = self.runtime.invoke_command(name, args).await? {
        if let Some(msg) = output.message {
            self.push_notification(&msg);
        }
        return Ok(false);
    }
}
```

### 7c. Autocomplete — include extension commands

`complete()` includes extension command names alongside the built-in `COMMANDS` list.

### 7d. Notification drain — poll runtime.notifications()

In the TUI ticker tick (or a new branch in the select! loop), drain
`runtime.drain_notifications()` and push each as a notification card to scrollback.

### 7e. Input event — fire before processing

When the user submits, before built-in dispatch:

```rust
let check = runtime.check_input(&raw_input).await?;
match check.action {
    InputActionKind::Handled  => return Ok(false),
    InputActionKind::Transform => { raw_input = check.text.unwrap_or(raw_input); }
    InputActionKind::Continue  => {}
}
```

## 8. phi-session/session_builder — Load Extensions at Build Time

In `build_session()`:

```rust
// Discover and load Lua extensions
let ext_paths = phi_lua::discover_extensions(cwd);
let lua_exts = phi_lua::load_extensions(&ext_paths);

// Build AppRuntime
let mut builder = RuntimeBuilder::new().with_cwd(cwd).with_session_id(&session_id);
for ext in phi_builtins::bundled_extensions() { builder = builder.register_boxed_extension(ext); }
for ext in lua_exts { builder = builder.register_boxed_extension(ext); }
let runtime = Arc::new(builder.build()?);
runtime.boot()?;

// Augment RlmConfig with extension stubs
let ext_stubs = runtime.extension_prelude_stubs();
let setup_code = format!("{}\n{ext_stubs}", CODING_PRELUDE);
let ext_tool_specs: Vec<_> = runtime.tool_specs().collect();
let tools_section = coding_tools_section(&ext_tool_specs);

// Wire runtime into RLM
let mut rlm = Rlm::new(config, ...).with_runtime(runtime.clone());
```

Pass `runtime` into `TuiSetup`.

## Lua Extension Example (for ori-mnemos bridge)

Once the above is implemented, an ori-mnemos bridge would be `.phi/extensions/ori.lua`:

```lua
phi.on("session_start", function(event)
  os.execute("ori orient --brief 2>/dev/null &")
end)

phi.on("before_agent_start", function(event)
  local ok, summary = pcall(function()
    local f = io.popen("ori status --short 2>/dev/null")
    local out = f:read("*a"); f:close(); return out
  end)
  if ok and summary and #summary > 0 then
    return { system_prompt_append = "\n\n## Memory Vault Status\n" .. summary }
  end
end)

phi.on("session_shutdown", function()
  os.execute("ori add --type insight --title 'phi session' >/dev/null 2>&1 &")
end)

phi.register_command("/recall", {
  description = "Query the ori memory vault",
  handler = function(args)
    local f = io.popen("ori query " .. args .. " 2>&1")
    local out = f:read("*a"); f:close()
    return { message = out }
  end,
  completions = function(partial) return {} end
})
```

(MCP client integration — exposing all 16 ori tools via `phi.register_tool` backed by
`ori serve --mcp` — is a separate follow-on plan once the above foundation is in place.)

## Cargo.toml Changes

- Add `crates/phi-lua` to workspace members
- `phi-lua` deps: `phi-ext`, `mlua` (workspace), `anyhow`, `serde_json`, `tokio`, `futures`, `dirs`
- `phi-runtime` deps: add `futures`
- `phi-rlm` deps: add `phi-runtime` (for `AppRuntime`)
- `phi-session` deps: add `phi-lua`, `phi-runtime`

## File Change Summary

| File | Change |
|------|--------|
| `crates/phi-ext/src/lib.rs` | Rewrite types (EventKind, EventPayload, EventReturn, async handler) |
| `crates/phi-lua/` | New crate: LuaExtension, discover_extensions, load_extensions |
| `crates/phi-runtime/src/lib.rs` | Async dispatch, filter_system_prompt, filter_messages, check_tool_call, filter_tool_result, check_input, drain_notifications, status_map |
| `crates/phi-builtins/src/prelude.py` | Add __phi_ext_tool__ helper |
| `crates/phi-builtins/src/prelude.rs` | coding_tools_section takes ext_tools param |
| `crates/phi-builtins/src/lib.rs` | Remove ScaffoldLifecycleExtension no-op scaffold |
| `crates/phi-repl/src/lib.rs` | Add SubcallRequest::ExtTool, SubcallResponse for it |
| `crates/phi-repl/src/driver.py` | Handle ext_tool subcall kind |
| `crates/phi-rlm/src/engine.rs` | Add AppRuntime field, fire new events, call filter_* |
| `crates/phi-session/src/session_builder.rs` | Load Lua extensions, build AppRuntime, augment RlmConfig |
| `crates/phi-session/src/tui.rs` | Wire AppRuntime into slash dispatch, input check, autocomplete, notification drain |
| `Cargo.toml` | Add phi-lua workspace member + deps |
