# phi Architecture Plan

A Rust harness whose core execution model is a **Recursive Language Model (RLM)** —
[alexzhang13/rlm](https://github.com/alexzhang13/rlm), [arXiv:2512.24601](https://arxiv.org/abs/2512.24601) —
wrapped in phi-grade infrastructure ported from [`ref/phi`](../ref/phi): unified provider layer,
OAuth, session trees, and an inline-scrollback TUI.

The RLM thesis, adopted wholesale: replace `llm.completion(prompt)` with `rlm.completion(prompt)`.
The root LM gets a REPL environment where the (potentially huge) context lives as a variable; it
programmatically inspects, decomposes, and recursively queries sub-LMs over it. CodeAct-style —
**code is the only tool surface; there is no JSON tool-calling loop.** The RLM is itself a
"language model" (text → text), which makes recursion literal.

## Locked decisions

| Decision | Choice |
|---|---|
| Execution model | RLM-only. No phi-style JSON tool loop. File/shell access = functions in the REPL, not tool schemas. |
| REPL backend #1 | Python subprocess behind a `ReplEnv` trait — matches the paper's prompts and models' strong Python priors. Lua (mlua) and Docker backends later. |
| Extensions | Builtins/first-party = native Rust traits (`phi-ext`). Third-party = embedded Lua (`mlua`), declarative table API. Data-only resources (skills, prompts, themes) are code-free files. |
| TUI | Custom phi-style inline renderer (line-diff `Vec<String>`, normal scrollback, DEC 2026). Centerpiece: root chat stream + **expandable live recursion tree**. |
| Providers first | Anthropic Messages (+ Claude Pro/Max OAuth PKCE) and OpenAI Responses (+ ChatGPT Codex OAuth). OpenAI-completions-compat dialect later. |
| Disk formats | Clean slate: TOML settings, own JSONL schema (sessions = trajectories), XDG dirs. |

## Crate layout

```
crates/
  phi-ai        # provider abstraction: types, streaming, providers, models registry, auth + OAuth
  phi-rlm       # RLM engine: LanguageModel trait, root loop, limits/budgets, sub-call routing,
               # trajectory/session persistence, compaction
  phi-repl      # ReplEnv trait + Python-subprocess backend (driver protocol); later: lua, docker
  phi-ext       # extension API: traits, event model, REPL-function/command registration
  phi-runtime   # host: registries, lifecycle dispatch, settings, resources, system prompt assembly
  phi-builtins  # REPL prelude helpers + core slash commands, registered through phi-ext
  phi-lua       # (phase 6) mlua extension host; doubles as optional Lua REPL backend later
  phi-tui       # terminal UI library: renderer, components, editor, markdown, tree view
  phi-cli       # argument parsing, mode resolution
  phi-app       # binary: wiring, interactive / one-shot / rpc modes
```

```
phi-ai ← phi-rlm ← phi-runtime ← phi-app → phi-tui, phi-cli
         ↑  ↖ phi-repl
      phi-ext ← phi-builtins, phi-lua
```

The scaffold's extension-first principle stands: `phi-runtime` owns registration and dispatch,
frontends stay thin, builtins prove the host API. Current sync string-payload stubs in
`phi-ext`/`phi-runtime` evolve to async with structured payloads.

---

## phi-ai — provider layer (ported from `ref/phi/packages/ai`)

Unchanged by the RLM pivot: the engine needs `complete()` for sub-calls and `stream()` for live
root turns in the TUI.

### Core types

```rust
enum Api { AnthropicMessages, OpenAiResponses /* later: OpenAiCompletions, … */ }

struct Model {
    id: String, name: String, api: Api, provider: String, base_url: String,
    reasoning: bool,
    thinking_levels: Option<ThinkingLevelMap>,  // off|minimal|low|medium|high|xhigh → provider value
    input: Vec<Modality>,
    cost: CostRates,                            // $/M: input, output, cache_read, cache_write
    context_window: u64, max_tokens: u64,
    headers: Option<HeaderMap>,
    compat: Option<Compat>,                     // per-Api quirk flags
}

enum Message { User(UserMessage), Assistant(AssistantMessage) }
struct AssistantMessage { content: Vec<AssistantContent /* Text|Thinking|ToolCall */>,
    usage: Usage, stop_reason: StopReason, error_message: Option<String>, … }
struct Context { system_prompt: Option<String>, messages: Vec<Message> }
```

(`ToolCall` content stays in the types because the wire protocols define it, but the harness
never drives a JSON tool loop.)

### Streaming

- `AssistantEvent` enum: `Start`, `Text/Thinking{Start,Delta,End}`, `Done{message}`,
  `Error{message}` — delta-only events plus an `Accumulator` helper (divergence from phi's
  per-event full snapshots; avoids clones).
- `EventStream`: mpsc-backed; `next().await` + `result().await`.
- **Contract (phi's): providers never fail out-of-band.** Network/HTTP/abort all become a
  terminal `Error` event with a synthesized `AssistantMessage` (`stop_reason: Error | Aborted`).
- Retries/backoff with `max_retry_delay_ms` cap in the provider layer.
- Context-overflow detection: port of phi's regex table + `usage.input > context_window` check.
- Transport: `reqwest` (rustls) + `eventsource-stream`.

```rust
trait ApiProvider: Send + Sync {
    fn api(&self) -> Api;
    fn stream(&self, model: &Model, ctx: &Context, opts: StreamOptions) -> AssistantEventStream;
}
```

Registry `HashMap<Api, Arc<dyn ApiProvider>>`; `stream_simple()` maps `ThinkingLevel` →
provider params (clamped per model). Models: embedded curated `models.json` via `include_str!`
+ user `models.toml` overlay; `calculate_cost()` fills `usage.cost`.

### Auth

Resolution priority per provider (port of phi's `auth-storage.ts`):
1. runtime override (`--api-key`) → 2. stored api_key → 3. stored oauth (auto-refresh,
file-locked) → 4. env (`ANTHROPIC_OAUTH_TOKEN` > `ANTHROPIC_API_KEY`; `OPENAI_API_KEY`).

Storage: `auth.json` (0600, `fd-lock`) in the data dir:
`{ "anthropic": { "type": "oauth", "access", "refresh", "expires" }, "openai": { "type": "api_key", "key" } }`

`OAuthProvider` trait: `login()`, `refresh()`, `api_key()`. Constants verified against `ref/phi`:

- **Anthropic** — Authorization Code + PKCE (S256). Authorize `https://claude.ai/oauth/authorize`,
  token `https://platform.claude.com/v1/oauth/token`, redirect `http://localhost:53692/callback`,
  client `9d1c250a-e61b-44d9-88ed-5944d1962f5e`, scopes `org:create_api_key user:profile
  user:inference user:sessions:claude_code user:mcp_servers user:file_upload`. Expiry minus 5 min skew.
- **OpenAI Codex** — PKCE via `localhost:1455` callback or device-code
  (`https://auth.openai.com/api/accounts/deviceauth/{usercode,token}`), client
  `app_EMoamEEZ73f0CkXaXp7hrann`, scopes `openid profile email offline_access`; account id from
  JWT claim `https://api.openai.com/auth.chatgpt_account_id` (decode, no verify).
- Device-code engine per RFC 8628: min 1 s interval, default 5 s, `slow_down` +5 s, cancellable.

Callback server: single-request responder on a loopback tokio `TcpListener`. PKCE via
`rand`+`sha2`+base64-url; browser via `webbrowser` with printed-URL fallback.

---

## phi-rlm — the engine (ported from `rlm/core/rlm.py`)

### The recursion primitive

```rust
#[async_trait]
trait LanguageModel: Send + Sync {
    async fn complete(&self, prompt: Prompt) -> Completion;   // Prompt: text or message list
}
```

Implemented by **(a)** plain provider-backed models (via phi-ai) and **(b)** `Rlm` itself.
`rlm_query` inside the REPL constructs a child `Rlm` at `depth + 1`; at `depth >= max_depth` it
falls back to a plain `llm_query`. Recursion is just trait recursion.

### Root loop (per `completion()` call)

1. Spawn (or reuse, if persistent) a `ReplEnv`; inject `context`, prelude, `answer` dict.
2. System prompt = port of `RLM_SYSTEM_PROMPT` (+ orchestrator addendum + custom-tools section +
   skills/context-file addenda from phi-runtime). Documents: `context` variable, `llm_query`,
   `llm_query_batched`, `rlm_query`, `rlm_query_batched`, `SHOW_VARS()`, `answer = {content, ready}`
   protocol, ~20 K-char output truncation, "plan in prose, one ```repl``` block per turn".
3. Per iteration (max 30 default): send `Turn {i}/{max}` user message → stream root assistant
   response → extract ` ```repl ``` ` blocks → execute in env → capture stdout/stderr (truncate
   ~20 K) → append formatted result as next user message.
4. Terminate when `answer.ready == true` (env reports answer state after each exec), or on
   guardrail: `max_iterations`, `max_budget` ($, cumulative across the recursion tree),
   `max_timeout`, `max_tokens` (total in+out), `max_errors` (consecutive). On guardrail stop,
   return best-partial answer if one was staged.
5. Cancellation: `CancellationToken` threaded into root stream, env exec, and all sub-calls.

### Config (mirrors upstream knobs)

`RlmConfig { root_model, sub_model /* one, like upstream */, max_depth (1), max_iterations (30),
max_budget, max_timeout, max_tokens, max_errors, max_concurrent_subcalls (4),
sampling_args / sub_sampling_args, custom_system_prompt, user_prologue, orchestrator (true),
persistent (false), compaction { enabled, threshold_pct: 0.85 } }`

### Sub-call routing

REPL code calls `llm_query(...)` → driver sends a `subcall` frame to the host → `LmHandler`
dispatches to the configured `LanguageModel` (plain or child-RLM) under a concurrency semaphore
(`max_concurrent_subcalls`) → result frame back to the blocked REPL call. Usage/cost from every
sub-call rolls up the tree for budget enforcement.

### Events (single stream feeding TUI, logger, extensions)

`RlmEvent`: `RunStart/End`, `IterationStart/End`, `RootDelta` (streamed text), `CodeExecStart/End`
(code, stdout, duration), `SubcallStart/End` (depth, model, prompt preview, cost, error),
`AnswerSubmitted`, `LimitHit`, `CompactionApplied`.

### Sessions = trajectories

One JSONL file per conversation (clean-slate schema v1), `data_dir/sessions/<project>/…`:

- Header: `{ type: "session", version: 1, id, cwd, timestamp, parent_session? }`.
- Entries (uuidv7 8-char ids, `parent_id` links → DAG): `user`, `root_assistant`, `code_exec`
  (code + truncated output + duration), `subcall` (nested tree: depth, model, prompt, completion,
  usage, children), `answer`, `model_change`, `compaction`, `label`, `leaf` (current tip).
- Multi-turn chat = persistent env + appended entries; resume/fork = leaf-pointer navigation
  (phi's session-tree mechanics, applied to RLM trajectories). The full recursion tree is
  reconstructable for the TUI tree view — this subsumes upstream's `RLMLogger` + visualizer.

### Compaction

Upstream semantics: when root message tokens reach `threshold_pct` (0.85) of the model's context
limit, summarize older root history; full history stays available to the model as a REPL
variable (`history`). Token estimates: chars/4 with last-usage baseline (phi's heuristic).

---

## phi-repl — environments

```rust
#[async_trait]
trait ReplEnv: Send {
    async fn start(&mut self, setup: EnvSetup) -> Result<()>;   // context payload, prelude fns
    async fn exec(&mut self, code: &str, timeout: Duration) -> ReplResult;
    async fn answer_state(&mut self) -> AnswerState;            // { content, ready }
    async fn shutdown(&mut self);
}
struct ReplResult { stdout: String, stderr: String, duration: Duration, truncated: bool }
```

### Python subprocess backend (first)

- Spawn `python3` running an embedded driver script (shipped via `include_str!`, written to a
  temp file). Persistent namespace across `exec` frames — REPL semantics, not Jupyter: only
  `print()` output returns.
- **Protocol: ndjson over stdio**, host ↔ driver (divergence from upstream's TCP `LMHandler`;
  stdio is simpler for a child process and keeps loopback closed):
  `exec {id, code}` / `exec_result {id, stdout, stderr, error}` /
  `subcall {id, kind: llm|rlm, prompt, model?}` / `subcall_result {id, text|error}` /
  `answer_state {…}`. Batched queries: driver issues N concurrent `subcall` frames (thread pool
  driver-side); host semaphore caps concurrency.
- Driver-side prelude: `llm_query`, `llm_query_batched`, `rlm_query`, `rlm_query_batched`
  (blocking shims over the protocol), `SHOW_VARS()`, `answer` dict, plus **host-registered
  functions** (extension/builtin REPL fns) auto-generated as shims that round-trip through the
  host.
- Per-exec timeout enforced host-side (kill + respawn on hang); env crash = consecutive-error.

### Later backends

- **Lua (mlua)** — in-process, zero-dep; host functions injected directly (no IPC). Requires a
  Lua variant of the system prompt; behavior-fidelity risk vs the Python-trained priors, so it
  stays the offline/embedded option, not the default.
- **Docker** — same Python driver inside a container (upstream `DockerREPL`); protocol unchanged.

---

## phi-ext + phi-runtime — extension kernel

`phi-ext` remains the single API for builtins, Lua extensions, and first-party features; payloads
stay serde-serializable. The RLM pivot reshapes the surface:

- **Registration:** `register_repl_fn(name, signature_doc, handler)` — the headline replacement
  for phi's `registerTool`. The function appears in the REPL (driver generates a shim) and its
  docstring is appended to the system prompt's custom-tools section; calls round-trip to the
  host handler. Also: `register_command` (slash commands), `register_shortcut`, `register_flag`,
  `register_provider`, `register_message_renderer`.
- **Hooks (events):** `session_start/shutdown`, `run_start/end`, `iteration_start/end`,
  `before_root_turn` (mutate root messages), `code_exec_start/end` (observe/veto code),
  `subcall_start/end`, `answer_submitted`, `before_provider_request/after_provider_response`,
  `input` (transform user input), `user_bash` (`!cmd` override), `model_select`,
  `resources_discover`, `compaction`.
- **Host handle:** `send_message`, `append_entry`, `set_model`, `exec`, session label/name,
  event bus — phi's `ExtensionAPI`, minus tool-loop affordances.

`phi-runtime` additionally owns:

- **Settings:** TOML, global `settings.toml` deep-merged with project `.phi/settings.toml`.
  Keys: default root/sub model + provider, thinking level, RLM limits (max_depth, max_iterations,
  budget, timeouts, concurrency), env backend, theme, compaction, retry policy, enabled models.
- **Resources:** skills (`SKILL.md` + frontmatter → system-prompt XML), prompt templates (`.md`,
  `$1 $@ ${N:-default}` substitution, exposed as slash commands), themes,
  `AGENTS.md`/`CLAUDE.md` context files (cwd→root walk + global) as `<project_context>` blocks.
- **System prompt assembly:** RLM base prompt + orchestrator addendum + custom-tools section +
  resources, per run.

## phi-builtins — REPL prelude + core commands

The seven phi tools dissolve into the environment (CodeAct). The driver prelude ships convenience
helpers implemented **in the environment** (so they work identically under Docker isolation),
with phi's truncation discipline as defaults:

| Helper | Notes |
|---|---|
| `bash(cmd, timeout=None)` | subprocess, streamed capture, 2000 lines / 50 KB truncation, overflow to temp file |
| `read_file(path, offset=0, limit=2000)` | 50 KB cap |
| `write_file(path, content)` / `edit_file(path, edits)` | edit = exact-then-normalized fuzzy match (port phi's matcher + tests) |
| `grep(pattern, path=".", …)` / `find(glob)` / `ls(path)` | thin wrappers over rg/fd/os with phi's limits |

(The model can always use raw Python instead; helpers exist for ergonomics and bounded output.)
Host-side, `phi-builtins` registers the core slash commands: `/model`, `/login`, `/logout`,
`/new`, `/compact`, `/fork`, `/tree`, `/resume`, `/settings`, `/export`, `/quit`.

## phi-tui — terminal UI library (ported from `ref/phi/packages/tui`)

- **Renderer:** components render `Vec<String>` (pre-styled ANSI); string-equality line diff →
  repaint `[first_changed, last_changed]` only, relative cursor moves, DEC 2026 synchronized
  updates; **inline in normal scrollback** (no alt screen); full clear + redraw on resize;
  ≥16 ms frame coalescing.
- **Components:** `Component { render(width) -> Vec<String>; invalidate() }` + `Container`,
  `Text`, `Markdown`, `Editor`, `SelectList`, overlay stack.
- **RLM chat stream:** root prose as markdown; ```repl``` blocks syntax-highlighted; REPL output
  blocks collapsed past N lines (expandable); **recursion tree nodes** under each iteration —
  live subcall status (spinner/done), depth, model, duration, cost; expand to view sub-prompt
  and completion. Status line: iteration i/max, depth, cumulative cost vs budget, tokens.
- **Input:** crossterm raw mode + events, kitty keyboard enhancement flags when supported,
  bracketed paste, SIGWINCH.
- **Editor:** grapheme-aware wrap (`unicode-segmentation`/`unicode-width`), Emacs bindings +
  kill ring, undo stack, large-paste placeholders, history drafts, autocomplete providers
  (`@` files, `/` commands) via inline `SelectList`.
- **Markdown:** `pulldown-cmark` → ANSI through theme style-fns, `(text,width)` cache; code
  highlighting via `syntect` callback. Themes = TOML in resource dirs; truecolor via `COLORTERM`.

## phi-cli + phi-app — surface and modes

```
phi [prompt…] [@context-file …]
  --context <path|->        # the long context → REPL variable (file, dir, or stdin)
  --model/--provider        --sub-model/--sub-provider     --thinking <level>
  --max-iterations N        --max-depth N    --budget USD  --max-tokens N  --timeout SECS
  --env python|lua|docker   -c/--continue    -r/--resume   --session-id    --fork <entry>
  --login <provider>        --logout         --list-models [pat]           --api-key K
  -e/--extension <path>     --skill <path>   --no-context-files
  --system-prompt/--append-system-prompt     --mode rpc    --export <path>
```

Modes: interactive TUI (default); one-shot (prompt arg + non-TTY stdout → print the final
answer — incidental, not a milestone); **RPC** (ndjson over stdio for SDK embedding):
`completion`, `abort`, `new_session`, `get_state`, `set_model`, `fork`, `navigate_tree`,
`export`, `reload_extensions` — `RlmEvent`s stream out as JSON lines. `--login/--logout` work
headless (divergence from phi's TUI-only `/login`).

## phi-lua — runtime extensions (phase 6)

`mlua` (vendored Lua 5.4; async + serde + send). Extension = declarative table; host walks it:

```lua
return {
  name = "web-tools",
  repl_fns = {
    { name = "fetch_url",
      doc = "fetch_url(url: str) -> str — fetch a URL and return readable text",
      handler = function(ctx, args) return ctx:http_get(args[1]) end },
  },
  commands = { stats = { description = "Show run stats", run = function(ctx) … end } },
  on = { subcall_end = function(ctx, ev) end },
}
```

Lua tables ↔ `serde_json::Value` via `LuaSerdeExt`; no raw `os`/`io` — capabilities flow through
the `ctx` host handle. Discovery: `*.lua` / `<dir>/init.lua` in global + project extension dirs.

## On-disk layout (clean slate)

```
~/.config/phi/settings.toml        # global settings
~/.local/share/phi/auth.json       # credentials, 0600, fd-lock
~/.local/share/phi/models.toml     # custom providers/models
~/.local/share/phi/sessions/…      # JSONL trajectories, per-project subdir
~/.local/share/phi/{extensions,skills,prompts,themes}/
<project>/.phi/{settings.toml,extensions,skills,prompts}/
```

Via `directories::ProjectDirs`. The CLI binary and XDG application name are both `phi`.

## Workspace dependencies (additions)

| Crate | Use |
|---|---|
| `tokio`, `tokio-util`, `futures`, `async-trait` | runtime, cancellation, streams, dyn-async traits |
| `reqwest` (rustls) + `eventsource-stream` | HTTP + SSE |
| `thiserror` | library errors (`anyhow` stays app-side) |
| `uuid` (v7), `jiff` | entry ids, timestamps |
| `directories`, `fd-lock`, `toml` | paths, locking, settings |
| `sha2`, `base64`, `rand`, `url`, `webbrowser` | OAuth/PKCE |
| `crossterm`, `unicode-segmentation`, `unicode-width` | terminal + text |
| `pulldown-cmark`, `syntect` | markdown + highlighting |
| `mlua` (lua54, vendored, async, serde, send) | phase 6 |

## Milestones

| Phase | Deliverable | Exit criterion |
|---|---|---|
| 0 | `phi-ai` types, `EventStream`, models registry; async-ify `phi-ext` payloads | unit tests over stream/accumulator |
| 1 | Anthropic Messages + OpenAI Responses providers (complete + stream), env/stored API keys | both providers answer a prompt from Rust |
| 2 | Auth storage + Anthropic PKCE + Codex OAuth (PKCE & device-code), `--login` headless | Pro/Max + ChatGPT subscriptions usable |
| 3 | `phi-repl` Python driver + ndjson protocol; `phi-rlm` root loop, limits, `llm_query{,_batched}`, answer protocol | **`rlm_complete()` parity with upstream quickstart** |
| 4 | Recursion (`rlm_query`, depth, budgets, semaphore), trajectory JSONL, persistent envs, resume/fork | multi-turn RLM chat persisted; recursion tree reconstructable |
| 5 | TUI: renderer, editor, markdown, chat stream + live recursion tree, core slash commands | daily-drivable interactive RLM |
| 6 | Extension kernel hardening, Lua host, `register_repl_fn`, skills/prompts/themes | sample Lua extension injects a REPL fn + hook |
| 7 | RPC mode, Docker env, Lua REPL backend, completions-compat dialect, HTML export | RPC harness drives a full run; compaction on |

## Parity & test strategy

- Port upstream's prompts verbatim (`RLM_SYSTEM_PROMPT`, orchestrator addendum, turn format) and
  pin them with golden tests; replicate `examples/quickstart.py` behavior as the P3 gate.
- Driver protocol: fixture-driven tests (scripted env ↔ host frames), hang/kill/respawn tests.
- Provider layer: recorded SSE fixtures offline; live tests gated on env keys.
- Port phi's edit-matcher and compaction cut-point tests; renderer line-diff goldens.
- Budget/limit unit tests: iteration caps, consecutive errors, best-partial-answer return,
  cross-tree cost rollup.
- Risks: Python driver deadlocks (blocking subcalls vs stdio framing), SSE quirks per provider,
  kitty protocol variance, Lua↔tokio bridging, prompt drift vs upstream as they iterate.

## References

- RLM repo: https://github.com/alexzhang13/rlm (and `rlm-minimal`)
- Paper: https://arxiv.org/abs/2512.24601 — Blog: https://alexzhang13.github.io/blog/2025/rlm/
- phi reference: `ref/phi` (local checkout, gitignored)
