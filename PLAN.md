# Phi Minimal Core Plan

Status: active. This document is the source of truth for Phi's direction. Future
sessions: read this before starting feature work, update checkboxes as work lands,
and reject work that violates The Rule below.

## North star

Phi is a **minimal terminal coding harness in the spirit of pi** (`ref/pi`), with
exactly three deliberate differentiators:

1. **Mux** — a first-class multi-session multiplexer (`phi-mux`, `phi-grid`).
2. **RLM** — a Recursive Language Model engine instead of a flat tool-call loop
   (`phi-rlm`, `phi-repl`): the model writes Python in a persistent REPL and
   recurses via `llm_query`/`subagent`.
3. **Greater extension surface** — a richer, load-bearing extension API than pi's
   (`phi-ext`, `phi-lua`, `phi-event`).

Everything else should be indistinguishable from pi's philosophy: small core,
no built-in permission theater, features arrive as extensions.

## The Rule

> **If a feature can be an extension, it must be an extension. Built-in features
> must be implemented through the same public extension API that user extensions
> use, and live in `phi-builtins`. PRs that hardwire extension-shaped features
> into core crates get rejected.**

Corollary: the extension API's test suite is `phi-builtins` itself. If a built-in
can't be expressed through the API, the fix is to grow the API, not to bypass it.

### Direction (2026-07-03): maximal extension surface

New standing decision: **expand the extension surface as much as possible.** The
long-term ideal is that even the mux mode and the RLM engine themselves run as
extensions on a minimal substrate — if mux and RLM *can* be extensions, that is
the preferred end state. The goal is to **ship Phi with amazing defaults**: mux
and RLM stay first-class differentiators and are enabled out of the box, but
they get there by being the flagship consumers of the extension API rather than
hardwired privileges of core. Whenever a capability the loop or the mux needs
can be expressed as extension API surface, grow the API and move the feature
across the seam. The "What is core" table below describes today's state, not
the destination — the direction of travel is to shrink it.

This supersedes the WS12.3 note that rejected abstracting the RLM loop behind an
extension path: that rejection now reads as *sequencing*, not doctrine. Near
term the loop stays engine-owned while the registry seam matures (bare core
must still boot); each workstream should nonetheless prefer cuts that move
logic toward the extension surface over cuts that entrench it in core.

**Update (2026-07-05): the sequencing gate is lifted.** WS1 (agent packs) is
now the active workstream carrying this direction: the agent itself — system
prompt, tool surface, turn protocol, execution environment — becomes
registerable configuration, with the RLM engine demoted to the shipped
default pack. The registry seam WS12.3a built is judged mature enough to
carry it.

### What is core

| Crate | Why it's core |
|---|---|
| `phi-rlm`, `phi-repl`, `phi-builtins/prelude` | The RLM differentiator: engine loop, guardrails, Python tool prelude |
| `phi-mux`, `phi-grid` | The mux differentiator |
| `phi-ext`, `phi-lua`, `phi-event` | The extension-surface differentiator |
| `phi-session` | Session tree/JSONL store — substrate everything depends on |
| `phi-ai`, `phi-ai-auth`, `phi-ai-types` | Provider abstraction, streaming, auth (keep thin — see WS7) |
| `phi-tui` | Single rendering substrate (see WS3) |
| `phi-app` | Entrypoint + minimal flag surface |

Also core-worthy despite not being crates: **context compaction** (part of the
loop — upstream rlm and pi both keep it in core) and **tool-prelude invariants**
(read-before-edit, truncation, spill). Minimal and well-made are not in tension.

### What is explicitly NOT core

- **Permission system / sandboxing.** Adopt pi's stance: Phi runs with full user
  permissions; containerization is the user's sandbox. The `ToolCall`
  interception event lets an extension implement approvals. Exception: project
  trust (WS8.1) — that gates *extension loading* and must work in core.
- **MCP** — build as an extension (WS10.1); it's the stress test for `register_tool`.
- **LSP, memory systems, hooks-as-config, share/export** — extension territory.
- **A 3,597-entry model catalog** — see WS7.2.

## Where we strayed (evidence inventory)

The drift is not size (pi's coding-agent package is ~49.5K lines TS; Phi is ~55K
Rust total). The drift is that **core stopped using its own extension API**:
`phi-builtins` registers exactly one extension (`DefaultOverlayExtension` +
visual defaults + the Python prelude) while everything else is hardwired:

- Slash commands are hardcoded string matches in `phi-session-tui`
  (`/fork`, `/model`, `/submodel`, `/usage`, `/login`, `/logout`, `/tree`,
  `/resume`, `/new`, `/help`, `/mo`, `/zzz`, `/exit`, `/quit`) instead of going
  through `phi.register_command`'s registry.
- Seven `--tools-*` display-tuning flags baked into `crates/phi-app/src/cli.rs:107-131`
  (padding, gap, max-lines, summary toggles) — presentation config in the binary's
  flag surface.
- Skills discovery (`crates/phi-session/src/skills.rs`) and system-prompt override
  files (`crates/phi-session/src/prompts.rs`) live in the *persistence* crate.
- Mux overlays (usage, model picker, help) are hardwired in
  `crates/phi-mux/src/ui/overlays/` despite `register_overlay` existing.
- `models.json` embeds 3,597 model entries (generated by `xtask`) but
  `registry.is_supported()` filters to 4 real backends
  (`crates/phi-ai/src/registry.rs:17-39`).
- `check_project_trust` (`crates/phi-ext/src/runtime.rs:290-310`) defaults to
  `trusted: true` with no handler — a cloned repo's `.phi/` Lua loads unprompted.
  This is below even pi's bar (pi gates project-local extensions behind trust).
- Hygiene: `crates/phi-builtins/src/prelude/__pycache__/*.pyc` are tracked in git.

### Second audit (2026-07-02) — what the dogfood pass missed

WS4's migrations are real, but they proved the **Rust** extension API, not the
one users actually get (Lua). The line was drawn correctly; it's enforced
asymmetrically. Findings, worst first:

- **Lua is a second-class extension surface.** The Lua bridge hardcodes
  `actions: Vec::new()` for command output
  (`crates/phi-lua/src/extension/mod.rs:381-384`), so a Lua command can never
  return a `UiAction` (quit, open picker, switch model, login…). Lua overlays
  get a fixed "bordered box of text lines" renderer, a fixed Esc/Enter/q key
  handler not driven by Lua at all, no `handle_scroll`, no payloads, no
  `OverlayResponder`. **Not one of the builtin commands or overlays migrated
  in WS4.1/4.2 could be reimplemented as a user extension today.** The Rule's
  corollary ("the API's test suite is `phi-builtins`") is currently testing an
  API users can't reach. (→ WS4.6)
- **The mux `:command` line is the disease WS4.1 cured, unaddressed**: a
  closed enum + literal string matches (`open`, `archive`, `cleanup`, `usage`,
  `refresh`, `quit`, `help`, nav aliases — `crates/phi-mux/src/keys/mod.rs:10-51`,
  dispatched from `app/commands.rs:329-385`), no registry. (→ WS4.7)
- **Three copies of the hardcoded per-tool-name render table**, not the one
  WS4.3 names: `phi-session-tui/src/tui/tool_display.rs:139-238`,
  `phi-app/src/tool_view.rs` (independently written, own bash special-casing),
  `phi-tui/src/tool_category.rs` (coarse categorizer). Core thereby knows the
  names of tools that only exist because `phi-builtins/prelude/*.py` defines
  them. (→ WS4.3, scope corrected)
- **"Bare core must boot" is unverifiable and false at the Cargo level**:
  `phi-app`, `phi-session-tui`, and `phi-mux` all hard-depend on
  `phi-builtins` (plain deps, no feature gate), and the repo has no CI.
  Deleting the crate is a compile error, not graceful degradation — though
  the runtime *behavior* mostly degrades correctly (overlay fallbacks,
  unknown-command errors). (→ WS4.5, made mechanical)
- **Two plumbed-but-dead APIs falsely signal coverage**:
  `register_message_renderer` (zero non-test consumers — already known,
  WS4.3) and `register_shortcut` (`list_shortcuts()` is never called by
  either frontend, while `phi-session-tui` hardcodes all 18 of its shortcut
  key checks with no keymap abstraction at all). Wire consumption or delete
  until needed. (→ WS4.3, WS3.3)
- **Feature vocabulary baked into the "generic" crate**: `UiAction`
  (`SwitchModel`/`StartLogin`/`TrustProject`), `PickerKind`,
  `OVERLAY_USAGE/MODEL/HELP` constants, `ModelPickerPayload`/`HelpPayload` —
  all in `phi-ext` (`command.rs:46-74`, `overlay_spec.rs:49-51,164-190`).
  Meanwhile `HostModalKind` (`phi-session-tui/src/tui/mod.rs:144-152`)
  duplicates the open-overlay-by-name path as a private two-variant side
  channel between the TUI and mux that no extension can participate in.
  Every new capability currently costs a phi-ext enum variant plus a match
  arm in each host. (→ WS4.8, decision needed before WS10)
- Minor: `Config.skills` (`crates/phi-session/src/config.rs:78-81`) stayed in
  the persistence crate's config schema after WS4.4 moved the feature to
  builtins — there's no primitive yet for extension-owned config sections.

What the audit confirmed is *right* (no churn needed): trust gating lives in
core with only the `/trust` convenience command in builtins (correct
direction); the prelude-as-core-inside-the-builtins-crate packaging is
deliberate and consistent with the core table; registration itself is
genuinely uniform (`Extension`/`ExtensionHost`/`RegistryHost` treat builtins
and Lua identically — the inequality is in the bridge, not the trait).

---

## Workstreams

**Numbering = priority (renumbered 2026-07-05).** Workstreams are ordered and
numbered by what matters now: open work first in priority order, landed work
after in its historical sequence. A session picking "the next open item in
numeric order" starts in the right place by construction. Commit messages and
notes from before 2026-07-05 use the old numbers; the map (old→new):
WS11→WS1 (agent packs), WS10→WS2 (style packs), WS6→WS3 (TUI),
WS1→WS4 (dogfood), WS0→WS5, WS2→WS6, WS3→WS7, WS4→WS8, WS5→WS9,
WS7→WS10, WS8→WS11, WS9→WS12 (agent loop). All references *inside this
document* have been rewritten to the new numbers, including inside dated
historical notes.

Current priority (2026-07-05): **WS1 — the agent itself becomes
configuration.** This supersedes the agent loop's 2026-07-03 queue-jump: its
quick wins landed, and its one open remainder (the 12.3b measurement run)
folds into WS1.5's acceptance, where the pi pack is the cleaner "native-only"
arm. In-flight WS2 remainders (2.3 packs, 2.4 acceptance) finish but spawn no
new dependents. Deferred UI work (WS4.3, WS3.2) stays deferred, though
WS3.2's case has strengthened — `tui/mod.rs` is ~3,270 lines after WS2.4, and
the transcript-cache state machine (six coordinated `Ui` fields plus a
caller-bumped generation epoch) is exactly the accretion 3.2 exists to
arrest.

Historical ordering decisions, compressed (rationale preserved in each
workstream's own notes): the dogfood pass (WS4) ran first as
highest-leverage; trust (8.1) ran early because dogfooding encourages people
to write project extensions; the 2026-07-03 loop audit queue-jumped the
agent loop (WS12) and ordered loop-surface API work before UI-side extension
work ("the loop is the product"); tool-surface *default* changes were gated
on measurement — that gate still stands, now inside WS1.5.

### WS1 — Agent packs: the agent itself as configuration (highest priority)

Decision (2026-07-05): the loop-as-extension direction stops being "eventual"
and becomes the active workstream. WS2 proves the *visual* surface by
imitation; WS1 proves the *agent* surface the same way. The goal: the entire
agent — system prompt, tool surface, turn protocol, execution environment —
is a registerable configuration ("agent pack"), selectable per session, and
the RLM engine is demoted from hardwired privilege to the shipped default
pack.

**The deliverable is the API, not any particular agent.** WS1 ships
primitives and grows seams; it does not design multi-agent features into
core. The test of done is expressibility: a user who wants an agent
architecture this plan never imagined — debate tournaments, MCTS over
subagents, a swarm with a scratchpad protocol — should find the primitives
sufficient and core unmodified. The four scenarios below are *falsifiers*
chosen to span the surface (peer communication, loop shape, tool surface,
turn protocol), not a feature roadmap; none is fully expressible today:

1. **Peer agents** — multiple agents talking to each other while working one
   problem. Today only hierarchical `subagent()` call/return exists
   (`router.py:342`, `engine/subcalls.rs`): no addressability, no messaging,
   no shared state between siblings beyond what the parent threads through.
2. **A pi-shaped agent** — flat tool-call loop, native read/edit/bash tools,
   no `repl` tool in the model's surface, no `context`-variable injection.
   Today ~80% expressible (WS12.3a native exposure + WS12.1 prose-stop +
   `SystemPromptMod` replace); the missing 20% is that `repl` is
   engine-hardcoded (`repl_tool_definition()`, `core.rs:97`) and the
   context scaffold is unconditional.
3. **Ask-an-agent** — a *user extension* builds claude-code's
   `AskUserQuestion` shape where the answerer is another agent, a
   deliberating pair, or the human as fallback (WS10.2's ask-the-user
   primitive). Hackable today via a `Handler` tool that spawns subagents;
   the clean form needs scenario 1's addressability. Phi does not ship
   this — it must merely be buildable.
4. **A codex-shaped agent** — codex's tool surface and turn protocol. The
   tool roster is registerable today (native-only exposure); stop semantics
   and parallel-tool policy are registerable after WS1.3; per-tool
   truncation and compaction policy are still engine-owned constants.

Every detail an agent pack cannot express is an API gap: grow
`phi-ext`/`phi-lua`/`phi-event`, never special-case a pack — the Rule's
corollary, third application (WS4: builtins, WS2: styles, WS1: agents).

Relationship to the feature-parity non-goal: unchanged in spirit, sharpened
in wording (see Non-goals). Phi does not *import* claude-code/codex features
into its defaults; it becomes expressive enough that their *shape* is a
config file. Parity-as-configuration is the differentiator;
parity-as-feature-set stays rejected.

- [x] **1.1 Agent-profile primitive.** `register_agent { name, … }` — the
      WS2.1 pattern (doctrine 5; `StyleSpec`'s inline-bundle precedent,
      `phi-ext/src/style_spec.rs`): a declarative bundle of the
      agent-defining knobs — `system_prompt` (full text or a named shipped
      identity), `tools` (roster of registered tool names + per-tool
      exposure override), `env` (the 1.2 block), `turn` (the 1.3 block),
      `subagent_defaults` (profile children spawn with). Exactly one
      profile active per session; selectable at session build (config.lua
      `agent` field, `--agent` flag) and at runtime (`/agent`,
      `UiAction::SwitchAgent` — mirrors `/style`; switching mid-session
      starts a fresh turn context, it does not rewrite history). The
      shipped `rlm` profile is the empty bundle — today's behavior with no
      special case, so "bare core must boot" is satisfied by the default
      profile rather than by hardwiring. System prompt becomes a
      *declarative* member here; the imperative
      `BeforeAgentStart`/`SystemPromptMod` hook stays for mutation
      (skills), but identity *selection* moves out of host code (today
      `CODING_IDENTITY` vs orchestrator addendum is chosen host-side in
      `phi_rlm::prompts`).
      *Landed as (WS1.1 slice, 2026-07-05):* `AgentSpec` in
      `phi-ext/src/agent_spec.rs` mirrors `StyleSpec` — a declarative
      bundle whose members (`system_prompt`, `orchestrator`, `tools`,
      `env`, `turn`, `subagent_defaults`) are all optional and fall
      through to the engine defaults. `register_agent` on
      `ExtensionHost`; `AppRuntime` stores agents + `active_agent` +
      `set_active_agent` (interior-mutable, exactly like styles).
      `UiAction::SwitchAgent { name }`; `/agent` command in
      `commands.lua` (mirrors `/style`); config.lua `agent` field +
      `Settings.agent`; `AgentStore` persistence
      (`phi-session/src/agent.rs`, the `StyleStore` layout). The shipped
      `rlm` default agent is the empty bundle (`defaults/agents/rlm.lua`,
      loaded via `from_source`); its absent `system_prompt` falls through
      to `CODING_IDENTITY` in `build_rlm_config` (session TUI) and
      `oneshot.rs` (one-shot), so today's behavior is the default profile
      rather than a hardwire. An agent that sets `system_prompt` overrides
      `CODING_IDENTITY`; a `.phi/SYSTEM.md` override still wins. Lua
      parity: `phi.register_agent(name, opts)` in `globals.rs`; the
      bridge collects + registers agents (`mod.rs`). The `tools`/`env`/
      `turn` members are defined on `AgentSpec` but not yet consumed by
      the engine — they are forward-compatible slots for WS1.2/1.3. Six
      tests cover activation, unknown-name rejection, duplicate
      registration, and serde round-trips. **Remaining for the 1.1+1.2
      refactor:** the `--agent` CLI flag (config-only today, matching
      `--style`); the `env.repl_tool`/`env.context` knobs landed in the
      WS1.2 slice below. Runtime `/agent` switching applies on the next
      session build (the note says so), not mid-turn.
- [x] **1.2 Demote `repl` to a default-profile tool.** `register_tools`
      (`phi-rlm/src/engine/rlm/core.rs:200`) hardcodes
      `repl_tool_definition()` ahead of the registry, and the Python env
      boots unconditionally. Fix: the profile's `env` block controls the
      *model-visible* surface — `repl_tool: false` removes the `repl` tool
      and its routing; `context: false` skips `context`/`context_0`
      injection. Deliberate scoping: "no repl tool" ≠ "no Python
      subprocess" — prelude functions remain the implementation substrate
      for `dispatch: Python` tools (one implementation, two exposures;
      hashline cache / read-before-edit / spill hold regardless of
      surface), so the pi-shaped profile keeps the battle-tested prelude
      under native-only exposure. The subprocess becomes lazy: boot on
      first Python-dispatch invocation, never for profiles whose roster is
      pure `Handler`. `answer`-dict protocol and scaffold restore gate on
      `env.repl_tool` (they're meaningless without the REPL surface).
      *Landed as (WS1.2 slice — model-visible surface, 2026-07-05):* the
      two surface knobs are wired into the engine. `AppRuntime` exposes
      `active_agent_env` / `active_agent_repl_tool` / `active_agent_context`
      (fall-through accessors mirroring `active_agent_system_prompt`).
      `register_tools` gates `repl_tool_definition()` on
      `env.repl_tool` (default on — bare core boots); `start_env` gates
      `context` / `context_0` injection on `env.context` (the payload
      still reaches the user prompt; only the REPL-side variable is
      gated); the exec-loop route check (`name == "repl" && repl_enabled`)
      stops a hallucinated `repl` call from routing to the REPL when the
      tool is hidden (it falls through to the unknown-tool synthetic-error
      path). Lua parity: the bridge collects the `env = { repl_tool = …,
      context = … }` block into `AgentSpec.env`; `AgentSpec::with_env` /
      `AgentEnv::no_repl_tool` / `AgentEnv::no_context` builders for
      programmatic use. Four engine tests cover repl-hidden, repl-default,
      context-skipped, context-default; one serde round-trip covers the env
      block. **Remaining for 1.2:** the lazy Python subprocess (boot on
      first Python-dispatch, skip for pure-`Handler` rosters) and the
      `answer`-dict / scaffold-restore gate on `env.repl_tool`. Both are
      engine-internal optimizations/invariants, not model-surface changes —
      the pi pack (WS1.5) is unblocked by this slice because its
      `repl_tool = false` / `context = false` already takes effect.
      *Landed as (WS1.2 slice — engine invariants, 2026-07-05):* the two
      remaining engine-internal items close the item. **Lazy Python
      subprocess:** `PythonRepl` gains a `lazy` flag (builder `lazy(bool)`);
      when set, `start` stores the setup but defers the `python3` spawn
      until the first `exec` / `answer_state` (auto-spawn on `proc.is_none()`);
      `shutdown` is a no-op when never spawned. `PythonEnvFactory` carries the
      flag, and `start_env` sets `lazy = !repl_enabled()` — so a profile that
      hides the `repl` tool defers the subprocess, and a pure-`Handler` roster
      (no `repl`, no `dispatch: Python` tool) never spawns a `python3`
      process at all. Bare core (repl on) keeps the eager spawn — today's
      behavior. **Answer-dict / scaffold-restore gate:** a single
      `Rlm::repl_enabled()` helper is now the one source of truth for
      `register_tools`, the exec-loop route check, `start_env`'s lazy
      decision, and the two new gates. In `run_loop`, the `final_answer` →
      `StopCause::Answer` completion path is skipped when `repl` is hidden
      (the model can't author `answer['ready']` without the REPL surface, so
      a Python-dispatch tool flipping the dict must not end the run — only
      prose / `<task_complete>` / default-answer paths complete a no-REPL
      run). In `RlmSession::send`, the warm-send `answer['content']='';
      answer['ready']=False` reset exec is gated on `repl_enabled()` (the
      scaffold-restore gate — meaningless without the REPL, and skipping it
      keeps a pure-`Handler` pi-profile session from booting the subprocess
      just to clear a dict it never sees). Two engine tests cover both: a
      pure-`Handler` pi-profile run pointed at a nonexistent interpreter
      completes via prose without ever spawning (eager boot would fail at
      `start_env`), and a Python-dispatch tool that flips `answer['ready']`
      with `repl` hidden does not stop the run (it continues to the prose
      turn). fmt + clippy (no new warnings) + nix workspace-test all green.
      **WS1.2 is closed.** The pi pack (WS1.5) is unblocked; `add_context`'s
      interaction with `env.context = false` (the session loads `context_N`
      via a Python exec regardless of the context knob) is left for the
      WS1.5 pi-pack pass to resolve if the pack needs it — it is a
      context-knob question, not a `repl_tool` one.
- [x] **1.3 Turn-protocol knobs, not user-supplied loops.** The loop stays
      one engine-owned `run_loop` parameterized by profile policy —
      loop-as-arbitrary-extension-code remains deferred until the knob
      vocabulary proves insufficient. Knobs, each with a reference
      precedent: stop condition (prose-stop / `answer['ready']` /
      explicit end-turn tool — codex `end_turn`), per-turn injection
      policy (WS12.2 left only capped-run warnings; profiles may add
      codex-style token-budget reminders), parallel tool execution
      (pi's per-file mutation queue, `agent-loop.ts:373-388`; codex's
      per-tool RwLock gating, `tools/parallel.rs`), per-tool truncation
      budgets (claude-code `toolLimits.ts`), and compaction policy
      (threshold + summary shape — WS9 machinery, profile-selected).
      Loop detection and the WS12.5 retry ladder are engine invariants, not
      knobs.
      *Landed as (WS1.3 slice — stop/reminders/parallel, 2026-07-05):* the
      three core turn-protocol knobs are wired into the engine.
      `AgentStop` enum (`Prose` / `Answer` / `EndTurn`) on `AgentTurn.stop`
      replaces the forward-compatible `Option<String>` slot. `stop =
      "answer"` suppresses prose-stop — a non-empty text turn is banked as a
      normal assistant message and the loop continues (the model must
      explicitly signal completion via `answer['ready']` /
      `<task_complete>`). `stop = "end_turn"` registers the engine-owned
      `end_turn` tool (`end_turn_tool_definition()` in `core.rs`); calling
      it completes the run with the best partial as the answer (trimmed).
      The prose and answer paths also still complete, so `end_turn` is
      additive. `per_turn_reminder = false` suppresses the remaining-turns
      reminder near the cap (the codex-shaped profile suppresses
      turn-budget noise); `None`/inherit keeps today's behavior (inject when
      approaching the cap). `parallel_tools = false` sequences tool
      execution: the stream completes first (the unbounded channel buffers
      calls), then exec drains and runs them serially in arrival order
      (codex's per-tool RwLock gating simplified to a global toggle);
      `None`/inherit keeps today's eager-during-stream behavior.
      `AppRuntime` accessors (`active_agent_stop`,
      `active_agent_per_turn_reminder`, `active_agent_parallel_tools`) and
      `Rlm` helpers (`stop_mode`, `per_turn_reminder_enabled`,
      `parallel_tools`) mirror the WS1.2 `repl_enabled` pattern. Lua
      parity: the bridge collects the `turn = { stop = "…",
      per_turn_reminder = …, parallel_tools = … }` block
      (`phi-lua/extension/mod.rs`), validating `stop` against the enum.
      `AgentSpec::with_turn` builder; `AgentTurn::end_turn()` convenience.
      Eight engine tests cover stop-answer prose suppression, end_turn
      tool registration + completion, default-agent prose-stop,
      per_turn_reminder on/off, parallel_tools serial completion; two Lua
      tests cover turn-block collection + invalid-stop rejection. fmt +
      clippy (no new warnings) + nix workspace-test all green.
      **Remaining for 1.3:** per-tool truncation budgets (claude-code
      `toolLimits.ts` — per-tool `maxResultSizeChars` overriding the
      engine's `max_output_chars`) and compaction policy (threshold +
      summary shape — WS9 machinery, profile-selected). These are deeper
      engine-internal knobs tied to the truncation/compaction subsystems;
      the three landed knobs are the model-visible turn-protocol surface.
      The pi pack (WS1.5) is unblocked by this slice.
- [ ] **1.4 Agent addressability + messaging.** The missing primitive under
      scenarios 1 and 3. Home per doctrine 3: the mux daemon already owns
      multiple sessions behind a versioned wire protocol
      (`crates/phi-mux/src/remote/protocol.rs`) — sessions become
      *addressable agents* (name + profile). New extension-API surface, one
      mechanism (doctrine 5) exposed on both tool surfaces:
      `spawn_agent(profile, prompt) -> address` (non-blocking, unlike
      `subagent()`), `send(address, message)`, and delivery into the
      target's next turn via the existing steer/`Context`-event path (no
      new history mutation mechanism). Peer pairs = two spawned agents
      given each other's addresses. Single-session hosts (plain `phi -p`)
      route within the local subagent tree so scripts don't require the
      daemon. Budget/usage roll-up follows the `subagent` precedent
      (`LmHandler` aggregation); depth/fan-out caps apply per
      `RlmConfig`. The commitment here is the *capability* (spawn without
      blocking, address, deliver); the exact names and payload shapes are
      implementation's call. Core ships no topology, no orchestration
      policy, no built-in multi-agent features on top — pairs, panels,
      supervisors, and whatever else are extension territory by
      construction. Explicitly out of scope: shared mutable blackboards —
      messages only, revisit with evidence.
- [ ] **1.5 Acceptance by expressibility.** Two halves, mirroring WS2.4:
      *Shipped defaults (dogfood):* `pi` and `codex` agent packs as pure
      Lua next to the WS2 style packs
      (`phi-builtins/defaults/agents/*.lua`) — verified on the wire, not
      the glass: run the `bench/` suite through each pack; the request
      stream must show the right roster (no `repl` tool in the pi pack, no
      Python-subprocess boot for pure-Handler profiles, codex-shaped stop
      behavior), with zero pack-specific code in core. This subsumes the
      open WS12.3b measurement: REPL-only vs hybrid vs pi-pack is the
      cleaner three-arm comparison, on models the user actually runs
      (still needs the user's model/spend call).
      *Expressibility (the actual point):* scenarios 1 and 3 — peer agents
      and ask-an-agent — implemented as *user-path* extensions (examples,
      not shipped defaults) using only the public API. Every piece of core
      they need that doesn't exist is an API gap to fix; every piece of
      core code written *for* them is a failure of the workstream.

Sequencing: 1.1 + 1.2 first (one refactor: profile plumbing + de-hardwiring
`repl`), 1.3 next, then the pi pack immediately as the falsifier; 1.4 can
proceed in parallel after 1.1 (it rides WS11's daemon, independent of loop
knobs); 1.5 closes the workstream.

### WS2 — Style packs: `/style` as the visual-surface acceptance test

Goal: Phi can *look like* the other harnesses purely through configuration —
`/style claude-code`, `/style codex`, `/style crush` (later `opencode`, …)
swap the entire visual identity at runtime, and `/style phi` swaps it back.
A style pack is an ordinary Lua extension bundling theme + spinner +
thinking expressions + tool renderers + message/transcript chrome; the
shipped packs live next to the WS4.9 embedded defaults
(`phi-builtins/defaults/styles/*.lua`) and load through the same
`LuaExtension::from_source` path. This is **presentation parity only** —
the feature-parity non-goal below stands untouched.

Why this is on-plan and not vanity: it's the visual counterpart of the
Rule's corollary. WS4.6/4.9 proved the *behavioral* surface (commands,
overlays, tools, events) is user-reachable; nothing yet proves the *visual*
surface is. "Make Phi indistinguishable from crush at a glance" is a
falsifiable acceptance test: every visual difference an imitation pack
cannot express is, by definition, an API gap — fix the API
(`phi-ext`/`phi-lua`/`phi-tui`), never special-case a pack. It also
subsumes the still-open WS4.3(b): mimicking three real transcripts is a
far better forcing function for the message-renderer design than inventing
one in the abstract.

- [x] **2.1 Style-pack primitive + runtime switching.** Today every visual
      registration is startup-time and last-write-wins; there is no
      activatable bundle and no way to re-skin a running session. Decide the
      mechanism deliberately (one declaration mechanism — doctrine 5): a
      `register_style { name, … }` bundle whose members are the *existing*
      registration shapes (theme, spinner, tool renderers, chrome), exactly
      one style active at a time, host re-resolves visuals + repaints on
      switch (both frontends; mux views re-theme per WS11.3's per-view
      palette note). Persist the choice in config so it survives restarts.
      Switching arrives as a `UiAction` from the command registry, not a
      side channel.
      *Landed as (2026-07-04):* `StyleSpec` (`phi-ext/src/style_spec.rs`)
      bundles the existing shapes **inline** — `Option<ThemeSpec>`,
      `Option<SpinnerSpec>`, `Option<ThinkingExpressionSpec>`,
      `Vec<ToolRenderSpec>` — deliberately not by-name references, so an
      inactive pack's theme never leaks into the `first_theme()` fallback
      order. Absent members fall through to the base registrations; the
      shipped `phi` default style is literally the empty bundle
      (`visual_data.lua`), so "switch back to phi" needs no special case.
      Runtime state: `active_style: Mutex<Option<String>>` on `AppRuntime`
      (the `connect_ui` interior-mutability precedent — the registry stays
      immutable, which bundle is active does not); `set_active_style`
      rejects unknown names listing the available ones, and the
      `active_theme`/`active_spinner`/`active_thinking_expression`
      accessors + a style-overlaying `tool_render_index()` replace the
      `first_*` calls in both frontends' `VisualContext` and palette
      paths. `UiAction::SwitchStyle { name }` (empty name = list styles)
      is emitted by `/style` (`commands.lua`) and `:style`
      (`mux_commands.lua`); the session TUI re-resolves palette (terminal
      colours kept on `Ui` for re-derivation), visual context, and tool
      render index, then repaints via the resize mechanics (clear
      viewport + `transcript.reset()` + cache flush); the mux re-derives
      `ScenePalette` and invalidates every view's surface+renderer (the
      alt-screen-transition mechanics). Persistence: user-global
      `style.json` (`phi_session::style::StyleStore`, the TrustStore
      layout) written on switch; config.lua grew a declarative `style`
      field, and `initial_style` (persisted-wins, approvals layering)
      applies at every runtime build — session, one-shot, and mux visual
      runtime. Lua parity shipped in the same change (WS4.6's lesson):
      `phi.register_style{...}` reuses the exact member parsers of the
      standalone registrations (now shared helpers in
      `phi-lua/extension/mod.rs`); dogfood tests in `lua_api.rs` +
      `defaults.rs`. Honest remainders: `/style` has no completion —
      packs register during the same runtime build as the command, so a
      static list can't know them (the WS7.2 /model gap; a dynamic
      completion primitive is WS2.4's registry-driven-completion work);
      in mux mode `/style` re-skins the issuing session and `:style` the
      chrome — other *live* sessions re-theme on their next build (live
      cross-runtime propagation rides WS11.3's per-view palette follow-up);
      Lua style themes are static palettes (no resolver), same as
      `register_theme`; text effects and message renderers aren't bundle
      members yet — grow the vocabulary when WS2.2's pack inventory
      demands it.
- [x] **2.2 Inventory the gap by imitation.** Same method as WS4.1/4.6:
      build the first pack (claude-code — closest to Phi's current look)
      and let every un-expressible detail become the work list. Expected
      gaps from eyeballing the refs, to be confirmed: transcript chrome
      (claude-code's `⏺`/`⎿` gutter bullets and collapsed tool results;
      crush's charm-style rounded borders, gradients, boxed tool panels;
      codex's flat minimal prefixes), input-box and status-line styling,
      welcome banner, tool-result collapse/expand presentation, and the
      WS4.3(b) custom message renderers. Grow the *declarative* vocabulary
      per WS4.3(a)'s decision — data, not closures on the hot render path.
      *Landed as (2026-07-04):* the pack —
      `phi-builtins/defaults/styles/claude_code.lua`, values read off
      `ref/claude-code` (darkTheme in `utils/theme.ts`, spinner chars +
      120ms cadence in `components/Spinner/`, gerund verbs in
      `constants/spinnerVerbs.ts`): claude-orange accent/shimmer theme
      (monochrome anchors via white `path`/`string` roles), the
      `·✢✳✶✻✽` asterisk-morph ping-pong spinner, ~60 whimsical thinking
      gerunds, and Bash/Read/Write/Update/Search/Glob/List tool verbs.
      Loaded via the same `from_source` path in both the session builder
      and the mux visual runtime; dogfood tests in
      `phi-lua/tests/defaults.rs` (activation + a drift test tying the
      pack's renderer roster to base `tool_render.lua`'s). **Expressible
      today** (worked first try): theme roles, spinner frames/cadence,
      thinking word lists, tool verbs/anchors/categories/bodies.
      **Gap inventory** (the work list for 2.3, worst first):
      1. *Tool-card chrome:* ~~not vocabulary members~~ **closed
         (2026-07-04)** by `ChromeSpec` (`phi-ext/src/chrome_spec.rs`), a
         new `StyleSpec` member (style-only, no standalone registration —
         chrome is a singular surface, so "which chrome" is exactly "which
         style"): `glyph_ok`/`glyph_failed` (status glyphs, palette-role
         coloured), `result_prefix` (first result row's gutter, claude-code
         `  ⎿  `; continuations hang at same-width blank indent),
         `more_lines` (`{n}` template), `container = card|flat` (flat
         drops the tinted repl/tool block surface *and* the pending
         boxed status — commit, replay/cache, and live paths all
         conditional, mirrored exactly). Pure data per WS4.3a — the
         in-flight label is still composed once at ToolStart, nothing
         extension-side runs on the tick path. `AppRuntime::active_chrome()`
         (style's chrome else `ChromeSpec::default()` = phi's stock look),
         snapshotted on `Ui` next to `tool_render`, re-resolved on
         `/style` switch.
      2. *Anchor format:* **closed (2026-07-04)** — `ChromeSpec.anchor_format`
         template over `{verb}`/`{args}` placeholders; `tool_identity`
         builds styled verb/args segments and composes through it (all
         branches: code-bodied, anchored, fallback `name key=value`);
         argless calls collapse to the bare verb, so `()` never dangles.
      3. *Status-line semantics:* **closed (2026-07-04)** —
         `ThinkingExpressionSpec.selection = cycle|per_turn` (per-turn
         holds one verb for the whole run, tools included, seeded by the
         host's `run_started_ms`; Lua: `thinking.selection`), plus
         `ChromeSpec.status_meta` for the dim `(elapsed · ↓ tokens)`
         suffix (claude-code thresholds: elapsed past 30s, tokens once
         > 0 — data from the host's usage, semantics from the chrome).
      4. *Text effects:* **closed (2026-07-04)** for the status line —
         `ChromeSpec.status_indicator = equalizer|spinner` (spinner mode
         draws the style's spinner frames in the accent role) and
         `status_effect = gradient|sweep` (`sweep` is claude-code's
         GlimmerMessage: `phi_tui::shimmer_sweep_text`, a ±1-char
         highlight sweeping right-to-left at 200ms/col with a 20-col
         runway, pulsing uniformly via `shimmer_flash_text` while a tool
         runs; base = accent, highlight = running role). Enum data, not
         closures — the `TextEffectSpec` handler surface stays unused on
         the tick path per WS4.3a.
      5. *Input box + welcome banner:* input box **closed (2026-07-04)**
         — `ChromeSpec.input_prompt` (glyph override, stock = Nerd Font
         chevron) and `input_ruled` (claude-code's two-rule box:
         top rule, editor, bottom rule, header re-reads as the footer
         line; selectors keep the stock layout). Welcome banner still
         open.
      6. *Custom message renderers* (WS4.3b) — still the right forcing
         function, still open. Meanwhile the *message chrome* half is
         vocabulary: `ChromeSpec.user_prefix` (ghost prefix replaces the
         accent gutter bar; continuations flow flush beneath, matching
         ink's inline wrap), `user_background` (full-width
         `user_message_bg` wash — the role the theme already carried,
         now consumed via `InlinePalette.user_bg`), `assistant_prefix`
         (claude-code's `⏺ ` dot, body hangs at prefix width).
      *Chrome consumption is session-TUI only, deliberately:* the plain
      CLI (`phi -p` event printing) keeps its own panel presentation —
      WS4.3a's doctrine is that frontends keep their presentation
      machinery, and the 2.4 at-a-glance test is the interactive
      transcript. Revisit only if one-shot output ever becomes a parity
      surface. The mux inherits chrome for free (it embeds the session
      TUI); mux *sidebar/statusbar* chrome stays host-owned.
- [ ] **2.3 Ship the packs.** `claude-code`, `codex`, `crush` as pure Lua
      in `defaults/styles/`; `opencode` as a stretch. The default remains
      Phi's own style; packs contain zero core special-cases (a grep for
      pack names outside `phi-builtins` should come up empty).
      *Progress:* `claude-code` v1 shipped with 2.2; inventory items (1)
      and (2) closed 2026-07-04 (`ChromeSpec`) and the pack now carries
      `chrome = { "⏺", "  ⎿  ", "{verb}({args})", "… +{n} lines (ctrl+o
      to expand)", flat }` — phi's expand toggle already *is* ctrl+o, so
      the marker text is literal claude-code. Items (3)–(5) closed later
      the same day (first 1:1 pass against the ref): the pack now also
      carries `status_indicator = "spinner"`, `status_effect = "sweep"`,
      `status_meta`, `input_prompt = "❯"`, `input_ruled`, `user_prefix =
      "❯ "`, `user_background`, `assistant_prefix = "● "`, and
      `thinking.selection = "per_turn"` — the asterisk-morph status line
      with the shimmering held verb, the two-rule input box, the gray
      user band with the ghost `❯`, and the `●` prose dot (BLACK_CIRCLE
      is `⏺` only on darwin; the pack targets `●`). Third pass (same
      day): `pending_indicator = "blink_glyph"` + `pending_note =
      "Running…"` (ToolUseLoader's 600ms visibility blink over the dim
      gutter note — in-flight rows animate by blink, not shimmer) and
      `repl_as_tool` (the repl container borrows the tool-card shape:
      `● Repl` verb anchor via the pack's `repl` renderer, code hanging
      2 cells under it, stdout/stderr behind a once-per-container `⎿`
      gutter — live streaming and replay paths mirrored). **Pack-content
      gaps** (zero Rust work — edit the Lua, not the API): ~120 of ref's 186
      spinner verbs are missing (the pack carries ~65, a deliberate subset —
      fill to full if/when the cadence feels thin); ~50 of 70 theme roles
      are unset (thinking levels, markdown chrome, tool backgrounds, chrome
      meta, glyph roles — all consumed by `apply_palette` `role!()` entries
      today, just not populated by the pack).
      *Closed (WS2.3 pack-content, 2026-07-04):* `find`'s verb is now
      "Search" (matching `GlobTool/UI.tsx:12`'s `userFacingName()` — both
      grep and glob label as "Search"), and all 9 `syntax_*` theme roles
      are now populated from `MONOKAI_SCOPES`
      (`native-ts/color-diff/index.ts:190`) so code blocks render with
      claude-code's Monokai palette instead of phi's default base16-ocean.
      **API vocabulary gaps** (need Rust, then Lua can express —
      per the WS2 method (§2.2): "every visual difference an imitation
      pack cannot express is, by definition, an API gap"):
      (e) diff color levels — phi has 2 of 6 (`md_diff_add`/`del` only;
      no `diffAdded`/`diffRemoved`/dimmed roles, renderer has one level);
      (f) `punctuation` syntax role — `SyntaxColors.punctuation` is
      hardcoded to `foreground` with no `apply_palette` `role!()` entry;
      (g) conditional `ToolRenderSpec.verb` — it's a single static
      `Option<String>` but ref's `userFacingName()` is input-dependent
      (FileEdit → "Update"/"Create"/"Updated plan", Bash → "SandboxedBash",
      FileRead → "Reading Plan"/"Read agent output") — needs a declarative
      rule vocabulary (data, not closures, per WS4.3a); (h) per-subagent
      colors — phi has 1 `subcall_accent`, ref has 8 — need roles +
      index-based renderer selection; (i) system-spinner color —
      `SpinnerSpec` carries no color (comes from `theme.accent`), ref has
      a second spinner color for system-level background work; (j)
      `OverlayPalette` has 8 of 70 roles — overlays can't access
      syntax/diff/thinking/warning colors; (k) `register_text_effect` is
      Rust-only (`traits.rs:78`, not in `globals.rs`) — a pack can
      *select* effects (`status_effect = "sweep"`) but can't *define* new
      ones or parameterize the sweep color per surface; (l) syntax
      scope-to-role mapping is hardcoded in Rust (`syntax_items()`,
      `constants.rs:198-271`) — the Lua can set 9 colors but can't
      change which TextMate scopes map to which roles or per-scope
      italic/bold styling. The crush `border` member (below) and
      WS4.3(b) message renderers are the other tracked vocabulary grows.
      **Style-out-of-scope deltas** (acknowledged, not API gaps):
      welcome banner (item 5's second half — a layout/content feature,
      not a color role), "ready"/"queue a follow-up…" placeholder copy
      (host-owned), `user_padding` widening the user band by one row
      each side (session config, not style), and phi's header line
      standing in for claude-code's footer info line (structural, not
      expressible via a style pack). **Feature gaps** (explicitly
      non-goal per the non-goals section below: presentation parity, not feature parity —
      these surfaces don't exist in phi, so the colors have nothing to
      paint): permission/auto-accept/plan-mode UI (6 ref colors), rate-
      limit bar (2), fast-mode indicator (2), mouse-selection highlight
      (1), message-action menu (1), memory view (1), ultrathink rainbow
      highlighting (7+7 shimmer) — each would need the feature built
      first, then a color role as a side effect. Next: re-check 2.4's
      at-a-glance test. `codex` and `crush` shipped 2026-07-05 as pure
      Lua in `defaults/styles/` (WS2.3): codex is flat-chrome with the
      braille spinner, `  └ ` tree-branch gutter, `› ` user prefix, and
      ANSI-cyan accent; crush carries the Charmtone Pantera palette
      (verbatim RGB from charmtone.go), `●`/`✓`/`×` tool glyphs, the
      `::: ` editor prompt, and card containers — its rounded-border
      member is a remaining API gap (no `border` on `ChromeSpec` yet;
      crush uses card bg-tinted boxes as the approximation). Gap (f)
      closed: `syntax_punctuation` is now a first-class `Theme` role
      (previously hardcoded to `foreground` in the syntect theme builder).
      `opencode` remains a stretch goal. Remaining API vocabulary gaps:
      (e) diff color levels, (g) conditional verb, (h) per-subagent
      colors, (i) system-spinner color, (j) overlay palette roles, (k)
      `register_text_effect` Lua exposure, (l) syntax scope mapping.
- [ ] **2.4 Acceptance.** Run one fixed scripted session (prompt → a few
      tool calls → prose answer) in Phi-with-pack and in the real harness
      (`ref/` checkouts), side by side: transcript, tool cards, spinner/
      status, and input area should be hard to tell apart at a glance.
      Mechanical half: packs load through the user extension path, a
      builtins-less build still boots with generic visuals, and `/style`
      completion is registry-driven (a user's own pack shows up uninstalled
      alongside the shipped ones).

### WS3 — TUI consolidation (investigative)

~25K lines across `phi-tui` + `phi-session-tui` + `phi-mux` + `phi-grid`.
Mux earns its keep (differentiator; `phi-grid`'s rect-level damage tracking is
finer-grained than zellij's). But:

- [x] **3.1** Audit rendering duplication between `phi-tui`, `phi-session-tui`,
      and `phi-mux` (markdown, code highlighting, diff, wrapping). Target: one
      substrate (`phi-tui`) that both frontends consume.
      *Audit (WS3.1, 2026-07-04):* the target is already met.
      `render_markdown`, `highlight_code`, `highlight_diff_line`, and
      `wrap_ansi` all live in `phi-tui` and are the only implementations —
      `phi-session-tui` consumes them directly (`tui/chat.rs`,
      `tui/tool_display.rs`, `tui/navigation.rs`); `phi-mux` embeds session
      TUIs and composites vt100 cells, so it does no markdown/code/diff
      rendering of its own. The one residual duplication noted in the WS6.3
      audit — `usage/rendering.rs`'s `display_cell_width`/`truncate_to_cells`
      — is a deliberate layering artifact, not drift: `phi-session` is the
      substrate crate *below* `phi-tui` in the dependency graph (phi-tui
      depends on phi-session, not the reverse), so the usage table can't
      import `phi_tui::cells`. The helpers are trivial (~30 lines, stable);
      folding them would require moving `cells.rs` out of phi-tui into a
      substrate crate, which is not worth it for a 3-function width utility.
      No work needed.
- [ ] **3.2** Break up `phi-session-tui/src/tui/mod.rs` (~3,270 lines) as part
      of the WS4 command/overlay extraction — it should shrink substantially
      for free.
- [x] **3.3** Adopt pi's keybinding discipline: no hardcoded key checks; every
      shortcut is a named, remappable binding (pi forbids this in AGENTS.md —
      copy that rule into ours once bindings are data-driven).
      *Status (2026-07-02 audit):* `phi-mux` is partway there — navigation
      actions route through `Keymap::lookup` with Lua `mux.keybinds`
      overrides, and extension-overlay toggle keys are data-driven; still
      hardcoded: the literal `':'` that opens the command line and the
      archive viewer's entire vim-ish keyset. `phi-session-tui` is unstarted:
      18 literal `KeyCode` checks (esc/ctrl-c/ctrl-l/ctrl-t/ctrl-g/tab/…),
      no keymap abstraction at all. Related dead API: `register_shortcut` is
      fully plumbed but `list_shortcuts()` is never called by either
      frontend — consuming it here is the natural fix (or delete it until
      this workstream needs it; a registered-but-unconsumed API falsely
      signals coverage).
      *Partial (WS3.3 shortcut consumption, 2026-07-04):* `list_shortcuts()`
      now has real consumers in both frontends — the session TUI's `/help`
      and the mux's `:help` overlay surface extension-registered shortcuts
      (`events.rs:show_help`, `commands.rs:help_entries`), so the API no
      longer falsely signals coverage. The full keybinding discipline
      (data-driven `Keymap` for the session TUI's remappable single-strokes,
      named bindings from config.lua's `keybinds` table) is now landed.
      *Landed (WS3.3, 2026-07-05):* the inline session TUI's 7 remappable
      single-stroke shortcuts (alt+up, ctrl+p/l/t/g/o, shift+tab) route
      through one `Keymap<SessionAction>` lookup in `events.rs`
      (`lookup_session_action`), driven by `BindingSpec` defaults overridable
      from config.lua's `[keybinds]` table (action name → binding string;
      unknown/invalid overrides fall back to defaults, mirroring the mux).
      The key-stroke parser (`KeyStroke`/`KeyToken`/`KeyModifiers`) moved to
      `phi_tui::keymap` and is shared with the mux, so both frontends use one
      binding-string grammar; the mux's private copy was deleted. The 4
      stateful shortcuts (double-Esc tree, double-Ctrl+C quit, Tab interrupt,
      Enter submit/login) stay hardcoded — they depend on multi-keystroke
      timing or editor state, not a single stroke. `cycle_model` keeps its
      shift-to-reverse direction (both `ctrl+p` and `shift+ctrl+p` bind to
      the action; the direction comes from the raw key's shift flag). The
      AGENTS.md rule (no hardcoded key checks for remappable shortcuts) is
      now added to the Code Conventions section.
- [x] **3.4 Transcript commit hardening.** `Transcript::committed_rows`
      (`phi-session/src/transcript/render.rs`) is a bare row count: anything
      that re-renders *already-committed* rows differently (suspects:
      `tool_summary` run-merging when a new ReplBlock extends a committed
      run; transcript-cache rebuilds) silently misaligns the commit point
      and corrupts scrollback from there on. Audit the prefix-stability
      invariant and assert it (or make commits content-addressed). Related,
      by design: resize resets `committed_rows` and re-commits the whole
      transcript below the stale copy (events.rs) — ordinary-CLI behavior
      on a real terminal, but noisy in mux panes that resize on layout
      changes; revisit if duplicates persist after the 2026-07-03 sanitize/
      DECAWM fix in `phi-tui/src/terminal.rs`.
      *Landed as (WS3.4, 2026-07-04):* commits are now **content-addressed**.
      On a cache miss (generation bump or key/len fingerprint divergence)
      the durable prefix is reassembled and `committed_rows` is clamped to
      the longest common row-prefix with the old assembly (`old.zip(new).take_while(==)`,
      capped at the new durable length). So changed rows re-emit as
      scrollback and the commit point never lands in the live region — live
      rows are never committed (the invariant the audit proved was
      violated by the old bare-count + coarse shrink guard when a gen-bump
      shrank the prefix but live content kept the total above the stale
      count, silently swallowing live rows). A `debug_assert!` in `render`
      catches any future regression. Four audit tests cover the suspect
      scenarios: content-change-same-key-len (display stale, commit
      intact), gen-bump-shrunken-prefix-with-live (no live swallowing),
      tool-summary run-merge, and monotonicity within a generation. The
      fast path (same generation + fingerprint match) keeps the
      append-only assembly and skips the divergence scan.

### WS4 — Dogfood pass: built-ins through the extension API (landed except 4.3)

Goal: `phi-builtins` becomes the proof that the extension API is sufficient.
Each migration that can't be expressed through the API is an API gap — fix the
API (`phi-ext`/`phi-lua`/`phi-event`), never bypass it.

- [x] **4.1 Unify the command registry.** One registry for slash commands; the
      TUI resolves commands only through it. Built-ins register at startup via
      the same path as `phi.register_command`. Migrate incrementally:
      - [x] Trivial: `/help`, `/usage`, `/exit`, `/quit`
      - [x] Session ops: `/new`, `/resume`, `/fork`, `/tree`
      - [x] Model ops: `/model`, `/submodel` (`/mo` was only ever a fuzzy
            completion query, not a command)
      - [x] Auth: `/login`, `/logout`
      - [x] Stragglers: `/trust` (new, WS8.1); `/zzz` was never a command
      Expected API gaps to surface: commands need to open overlays/pickers,
      read usage totals, mutate session state, trigger auth flows. Grow the
      command `ctx` to cover these (pi's `ctx` is the reference:
      `ref/pi/packages/coding-agent/docs/extensions.md`).
      *How the gap was closed:* `CommandSpec` grew `args_hint` +
      `arg_completions`; `CommandOutput` grew `actions: Vec<UiAction>`
      (quit, new-session, open-picker, switch-model, login/logout, help,
      trust) — handlers return declarative actions, the TUI interprets them
      in one `apply_action`. Built-ins live in
      `phi-builtins/src/commands.rs`; the TUI has zero hardcoded command
      strings and completion/help are registry-driven.
      *Caveat (2026-07-02 audit):* this proved the Rust API only. The Lua
      bridge discards `actions` entirely, so none of these commands is
      expressible as a user extension — see WS4.6. Stays checked because the
      migration described here is done; the parity gap is tracked separately.
- [x] **4.2 Overlays.** Move mux usage/model/help overlays to builtin extensions
      via `register_overlay`. Same drill: gaps in the overlay API get fixed in
      `phi-ext`, not worked around.
      *How the gap was closed:* `OverlaySpec.init_state` now takes an optional
      `OverlayPayload` (typed per overlay: `ModelPickerPayload`, `HelpPayload`);
      `toggle_key` became optional (programmatic-open-only overlays);
      `handle_scroll` added for wheel support; `DrawContext` grew `palette()`
      (`OverlayPalette` + `brighten`) so extension chrome matches the host
      theme; `OverlayResponder` (fires `Ok(None)` on drop) carries the model
      picker's reply so a dismissed/replaced picker never strands the
      requesting session. The mux opens overlays by name
      (`open_extension_overlay`/`toggle_extension_overlay`,
      `phi_ext::OVERLAY_USAGE`/`OVERLAY_MODEL`/`OVERLAY_HELP`) from `:usage`,
      `:help`, `AppAction::Help`, and `HostModalRequest`; usage/model/help
      live in `phi-builtins/src/overlays/`, and a build without them degrades
      to status notes / error replies. The archive viewer (with its
      `Restore(PathBuf)` host action) is the only overlay still hardwired.
      *Caveat (2026-07-02 audit):* Rust-API-only, same as 4.1 — Lua overlays
      are reduced to a fixed text-box renderer with close-only keys, no
      payloads, no responder (WS4.6). And `HostModalKind` grew alongside this
      work as a private TUI↔mux side channel that bypasses the by-name
      overlay path it duplicates (WS4.8).
- [ ] **4.3 Themes / spinners / renderers.** Default theme, spinner frames, and
      any special-cased message renderers become `register_theme` /
      `register_spinner` / `register_message_renderer` calls in `phi-builtins`.
      - [x] Registration side: `DefaultVisualExtension`
            (`phi-builtins/src/visual.rs`) registers the default theme
            (resolver over `generate_theme`), spinner, thinking expressions,
            and text effects; spinner frames now reference
            `phi_tui::SPINNER_FRAMES` instead of a hand-copied array.
      - [x] Consumption side unified: the inline session TUI derives its
            palette from `runtime.first_theme()` (previously only the mux
            did); the role-name→`Theme` overlay table moved to
            `phi_tui::apply_palette` (one copy, both frontends convert
            `ExtRgb` at the edge); the mux sidebar spinner goes through
            `VisualContext` instead of calling `phi_tui::spinner_frame_at`
            directly.
      - [x] (a) Per-tool render hooks: the hardcoded match on
            `bash`/`read_file`/`write_file`/`edit_file`/`grep`/`find`/`ls`
            becomes registry-driven, all **three** copies of the table
            (2026-07-02 audit) consuming one registry.
            *Landed as (2026-07-03):* `register_tool_renderer` +
            `ToolRenderSpec` in `phi-ext/src/tool_render.rs` — deliberately
            **declarative data, not pi-style `renderCall`/`renderResult`
            closures** (the design decision this item flagged): verb,
            display name, summary category, styled anchor args
            (`path`/`pattern` roles, `prefix`, `skip_values`), body kind
            (`args`/`none`/`code {key, lang, prompt}`), result-lang
            (`fixed`/`path_arg`), plus an `exit_code_trailer` flag for the
            prelude bash output convention. Rationale: three heterogeneous
            consumers (ANSI TUI cards, plain CLI panels, category tallies)
            project the same facts differently, and the in-flight status
            line re-renders every animation tick — closure hooks would put
            extension VMs on the hot render path, and pi's component-return
            hooks presume a single TUI framework Phi doesn't have. All
            three tables now consume the registry: `tool_display.rs`
            (`tool_identity`/`tool_title`/`tool_preview_lang`) and
            `tool_view.rs`/`render.rs` take `Option<&ToolRenderSpec>` and
            keep the old generic `name key=value` rendering as the
            no-spec fallback; `phi-tui/src/tool_category.rs` is deleted
            (`tally` + `ToolRenderCategory` live in phi-ext, categories
            come from specs, unknown → `other`). The 7 prelude helpers'
            specs live in `phi-builtins/src/tool_render.rs`
            (`ToolRenderExtension`, drift-tested against
            `coding_tools_section`); the core `repl` spec is seeded by
            `ToolRenderIndex` itself, so a builtins-less build still
            renders the engine's own tool richly and everything else
            degrades to generic anchors — the same bare-core story as
            commands/overlays. Registration is last-write-wins (provider
            precedent) so a user extension can restyle a builtin —
            covered by a Lua override test; Lua gets the full vocabulary
            via serde (`phi.register_tool_renderer`, dogfood tests in
            `phi-lua/tests/lua_api.rs`). Fixed on the way: the CLI
            `write_file` panel never showed its path (no title suffix and
            stripped from args); CLI Input/Args sections are now
            data-driven (shown iff residual args survive anchor
            stripping) instead of name-gated.
      - [ ] (b) A `Custom` transcript-entry variant dispatched through
            `get_message_renderer`, plus a `send_message`-style producer —
            `register_message_renderer` stays wired-but-dead (zero
            non-test consumers, no way for an extension to inject a
            custom-typed message) until this lands. Styling decision for
            *this* half is still open (renderer returns markdown? spans of
            `StyledText` with palette roles, per the WS4.6 overlay
            precedent?) — don't rush it.
- [x] **4.4 Skills.** Relocate `phi-session/src/skills.rs` into a builtin
      extension (discovery of `SKILL.md` dirs, injection into the system prompt
      via the `before_agent_start`-equivalent event). Skills are not RLM-core.
      *Landed as:* `phi-builtins/src/skills.rs` — `SkillsExtension` discovers
      at `register()` time (same pre-raw-mode timing as before) and injects the
      `<available_skills>` block via `BeforeAgentStart` →
      `SystemPromptMod::append`; no API gap surfaced. `phi-rlm` lost `Skill`,
      `RlmConfig::skills`, and the builder's `available_skills` section (the
      block now lands at the end of the prompt instead of before `env_info`);
      `phi-session` is skills-free. Side effect that closed a real gap:
      one-shot mode now builds the same extension runtime as the interactive
      modes (`session_builder::build_runtime` made shared) — previously it
      loaded no extensions at all, so Lua event handlers now apply to
      `phi -p` runs too. Trust gating unchanged (`include_project` flag,
      resolved by the host before construction).
      *Leftover (2026-07-02 audit):* `Config.skills` still lives in
      `phi-session`'s config schema (`config.rs:78-81`), consumed only by the
      builtin. Symptom of a missing primitive: extension-owned config
      sections. Fold into whatever WS4.8 decides, or accept and document.
      *Follow-on (2026-07-05): skills as slash commands.* Each discovered
      skill is also registered as a `/<skill-name>` session command whose
      handler returns a `SendPrompt` action (the SKILL.md body,
      frontmatter stripped, skill directory prefixed, user args appended)
      — the Claude-Code dual-entry design: a skill is both model-invoked
      (via the `<available_skills>` system-prompt index) and
      user-invoked (via the slash command), same `send_prompt` path as a
      typed prompt (steers into a running turn instead of racing a second
      one). API growth, not hardwiring: a new `UiAction::SendPrompt`
      variant in `phi-ext` (consumed by the session TUI; the mux host
      reports it unsupported, like the other session-only actions) and a
      `skill = true` flag on `phi.register_command` that makes a name
      collision with a built-in command a soft skip (warn + continue)
      rather than a build-breaking error, so a skill named `help` doesn't
      break the session build. All in `skills.lua` (the builtin extension)
      + the Lua bridge; zero core changes beyond the `SendPrompt`
      variant. Four tests cover slash-command registration, argument
      appending, frontmatter stripping, and the builtin-collision skip.
- [x] **4.5 Acceptance (made mechanical, 2026-07-02):** `phi-session-tui`
      contains zero hardcoded command strings (✓ done); `phi-mux` contains
      zero hardwired overlay feature logic (✓ done — WS4.7 closed both the
      `:command` parser and the archive viewer); deleting `phi-builtins`
      from the build leaves a bare-but-functional harness (engine + REPL +
      plain TUI + mux shell).
      *Audit finding:* the last clause is currently false at the Cargo level —
      all three app crates hard-depend on `phi-builtins` with no feature
      gate, and there is no CI, so the doctrine ("bare core must boot") is
      prose, not a property. *Status (WS4.5 sub-items landed):* the feature
      gate + CI check below are done, so "deleting phi-builtins from the
      build" is now mechanical (`--no-default-features`); with WS4.7 landed,
      all three clauses hold and the bare build stays CI-enforced. In a
      builtins-less build the mux `:` line has no commands (every input is
      an "unknown command" note) and no overlays — the shell itself
      (sidebar, sessions, keybound navigation) still works. Do this before
      more migration work:
      - [x] Gate builtin *extension registration* behind a cargo feature
            (default-on). The prelude splice stays unconditional —
            `phi-builtins/prelude` is core per the table above; if the
            feature split gets awkward, extract the prelude to its own
            module/crate rather than gating it.
            *Landed as:* `builtins` feature *inside* `phi-builtins`
            (default-on) `#[cfg]`-gates the extension modules
            (catalog/commands/overlays/skills/visual); the prelude module
            stays unconditional, so `phi-builtins` remains a non-optional
            dep everywhere. `phi-app`/`phi-session-tui`/`phi-mux` each
            expose their own default-on `builtins` feature forwarding down
            the dep chain, and the registration call sites are
            `#[cfg(feature = "builtins")]` with graceful fallbacks (no
            extensions registered, catalog model names drop out of `/model`
            and the mux picker chains).
      - [x] Add a builtins-less build to the flake checks so the acceptance
            criterion is CI-enforced, not re-audited by hand.
            *Landed as:* `checks.<system>.phi-app-no-builtins` — a crane
            `cargoBuild` of `-p phi-app --no-default-features` (build only,
            shares `cargoArtifacts`), visible to `nix flake check`.
- [x] **4.6 Lua parity for commands and overlays.** The Lua bridge exposes
      only a narrow slice of what builtins use: commands can't return
      `UiAction`s (`phi-lua/src/extension/mod.rs:381-384` hardcodes
      `actions: Vec::new()`); overlays get a fixed text-box renderer, a fixed
      Esc/Enter/q handler (Lua has no `handle_key` hook at all), no
      `handle_scroll`, no payloads, no `OverlayResponder`; Lua-registered
      shortcuts are no-op stubs ("handled at the TUI layer"). Themes,
      spinners, tools, providers, message renderers, and the full event
      surface *are* faithfully bridged — the gap is concentrated in exactly
      the two capabilities WS4.1/4.2 leaned on. Without this, the third
      differentiator ("greater extension surface") is only true for people
      writing Rust in this repo, and the Rule's corollary tests the wrong
      API. Dogfood test, same method as WS4.1: port one builtin command that
      returns `UiAction`s and one builtin overlay to Lua; every failure is
      the work list.
      *Landed as:* commands — the bridge returns the handler's full result
      as JSON and `parse_command_output` decodes `message` + `actions`
      (serde `UiAction` wire shape: `{ type = "OpenOverlay", data = {...}
      }`; malformed actions are hard, named errors), plus `context =
      "mux"`, `aliases`, and static `completions` so the whole
      `CommandSpec` surface is reachable. Overlays — a real protocol over
      the Lua thread (`phi-lua/src/extension/overlay.rs`): per-instance
      state lives in the VM (`init(payload)` return value, freed on host
      drop); `render(state, ctx)` gets `{cols, rows, palette}` and returns
      draw ops (`box`/`fill`/`text`/`clear`/`scrollbar`, colours as rgb
      arrays or palette role names) with the legacy line-array mode still
      drawing the (now palette-themed) text box; `handle_key(state, key,
      cols, rows)` returns `nil`/`"close"`/`{submit}`/`{respond}`/
      `{message, actions}` → `OverlayOutcome` incl. `Complete`;
      `handle_scroll` bridges; typed payloads cross as tagged tables
      (`kind = "model"|"help"`) with the `OverlayResponder` held host-side
      — `{respond = ...}` fires it, reply-on-drop survives. Dogfood tests
      port a `/usage`-style command, the help overlay, and a model picker
      (responder round-trip + dismissal) to Lua in
      `phi-lua/tests/lua_api.rs`. Shortcuts stay no-op stubs by design —
      neither frontend consumes `list_shortcuts()` yet; that's WS3.3's
      keymap work, not a bridge gap.
- [x] **4.7 Mux `:command` registry.** The mux colon-command line is a closed
      enum + literal string matches (`phi-mux/src/keys/mod.rs:10-51`) — the
      exact pattern WS4.1 removed from the inline TUI, missed by the original
      inventory. Route it through the same `CommandSpec` registry (namespace
      or context-tag mux-level commands vs session-level ones). The archive
      viewer overlay (`ui/overlays/archive.rs`, `Restore(PathBuf)` host
      action, own hardcoded keyset) migrates to `register_overlay` as part of
      this — it needs whatever host-action mechanism WS4.8 lands.
      *Landed as (part A):* `CommandSpec` grew `context: CommandContext`
      (`Session` default / `Mux`) and `aliases`; the colon commands live in
      `phi-builtins/src/mux_commands.rs` and return declarative actions —
      `UiAction::OpenProject` and the generic `UiAction::RunHostAction
      {name}` ("cleanup", "refresh", "reload", nav) join the vocabulary;
      the mux dispatches via `invoke_command_sync_in(CommandContext::Mux, …)`
      and interprets actions in `apply_mux_ui_action`. The `AppAction` closed
      enum survives only for *keybound* navigation (keymap territory, WS3.3),
      not command dispatch.
      *Landed as (part B):* archive persistence moved to
      `phi_session::archive` (pure store-layout code — list/archive/restore/
      cleanup); the viewer is now the `phi:archive` builtin overlay
      (`phi-builtins/src/overlays/archive.rs`) opened by name from
      `:archive`. API gap closed: `OverlayOutcome::Complete(CommandOutput)`
      — an overlay hands the host the same message+`UiAction`s vocabulary
      command handlers return (host applies actions first, then posts the
      message, so the overlay's status wins). Restore itself happens inside
      the extension via `restore_session`; the host only interprets
      `RunHostAction("reload")`. `phi-mux/src/ui/overlays/` is deleted
      entirely — the mux has zero hardwired overlay feature logic.
- [x] **4.8 Decide the `UiAction` vocabulary stance (before WS10).** Every
      host effect is a feature-named variant in `phi-ext` (`SwitchModel`,
      `StartLogin`, `TrustProject`, `PickerKind::…`) interpreted by a match
      arm in each host — extensions cannot define new host effects, and
      `HostModalKind` grew as a parallel private channel for the same job.
      Decision to make deliberately, not by drift: keep the closed enum for
      genuinely host-owned effects (quit, new-session — declarative actions
      match the snapshot-in/actions-out doctrine), but add a generic
      `UiAction::OpenOverlay { name, payload }` escape hatch that subsumes
      `ShowHelp`, most of `PickerKind`, and `HostModalKind` entirely. WS10.1
      (MCP) and WS10.2 (approvals) will hit this immediately; settling it
      first avoids two more rounds of enum growth.
      *Landed as:* `UiAction::OpenOverlay { name, payload: Option<serde_json::Value> }`
      subsumes `ShowHelp` and `PickerKind::{Usage, Model}` (both deleted);
      `PickerKind` keeps only the host-owned `Resume`/`Fork`/`Tree`.
      `/help`, `/usage`, `/model`, `/submodel` now emit `OpenOverlay` with the
      `OVERLAY_HELP`/`OVERLAY_USAGE`/`OVERLAY_MODEL` names; the session TUI
      routes it through one `open_overlay` fn (host-forward first, native
      selector fallback standalone) and the mux resolves it through
      `open_extension_overlay`. `HostModalKind` is deleted —
      `HostModalRequest` now carries `{name, payload}` by-name; the mux
      attaches the native `ModelPickerPayload` (model list + single
      `OverlayResponder`, reply-on-drop preserved) for `OVERLAY_MODEL` and
      routes any other name generically to every focused view.
- [x] **4.9 Defaults as embedded Lua.** (landed 2026-07-03; retro-added to
      the plan) The builtin defaults are now *literally* user extensions:
      commands, mux commands, tool renderers, coding tools, spinner/thinking
      data, about/help overlays, skills, and approvals ship as Lua in
      `phi-builtins/defaults/*.lua`, compiled in via `include_str!` and
      loaded through `LuaExtension::from_source` — the same path user `.lua`
      files take, registered first so a user's file can override. Honest
      Rust exceptions (need native resources): catalog (728KB embedded
      JSON), theme resolver + text effects, usage/model/archive overlays,
      MCP. Primitives grown on the way: `phi.fs`, `dispatch = "python"` in
      Lua `register_tool`, `LuaExtension::from_source`. Dogfood tests in
      `phi-lua/tests/defaults.rs`.

### WS5 — Codify the rule (landed)

- [x] Add "The Rule" and the core/not-core tables to `AGENTS.md` (or link there
      to this file) so every future agent session enforces it.
- [x] Remove `crates/phi-builtins/src/prelude/__pycache__/` from git; add
      `__pycache__/` to `.gitignore`.
- [x] Land the in-flight uncommitted work first (strict prose policy +
      `message()` primitive) — it's engine hardening, on-brand, and rebasing the
      dogfood pass over it is easier than the reverse.

### WS6 — Prune the surface

- [x] **6.1** Delete the seven `--tools-*` flags from `cli.rs`; move to settings
      (config file), keeping CLI flags for behavior only (model, budget,
      iterations, output mode, mux, project). Extensions can add flags via
      `register_flag`.
      *Landed as:* pure deletion — the `tools = {...}` table in `config.lua`
      already covered every knob, so the flags and the seven `tools_*`
      passthrough fields on `CliArgs` just died; `Settings::resolve` reads
      tool display settings from config only.
- [x] **6.2** Relocate `phi-session/src/prompts.rs` (`.phi/SYSTEM.md` overrides)
      next to the prompt builder in `phi-rlm/src/prompts/`, leaving `phi-session`
      as pure persistence (config, transcript, usage, store, tree).
      *Landed as:* `phi-rlm/src/prompts/overrides.rs`, re-exported as
      `phi_rlm::discover_prompt_overrides`; the trust-gated `include_project`
      contract is unchanged.
- [x] **6.3** Audit remaining `phi-session` contents for other non-persistence
      logic.
      *Found and relocated:* `files.rs` (`@`-token editor file completion —
      TUI input logic) → `phi-session-tui/src/files.rs` (took the `ignore`
      dep with it); `tool_summary.rs` (tool display categorization) →
      `phi-tui/src/tool_category.rs`, whose shared `tally` also replaced the
      duplicate tally loops in `phi-app/src/event_print.rs` and
      `phi-session-tui/src/tui/chat.rs`. *Stays, per the blessed list:*
      config, session store/tree, transcript, trust, usage. *Noted for
      WS3.1:* `usage/rendering.rs` re-implements `phi-tui`'s cell-width/
      truncation helpers — fold when auditing rendering duplication.

### WS7 — Slim the provider layer

- [x] **7.1** Keep the four real backends (`AnthropicMessages`, `OpenAiResponses`,
      `OpenAiCompletions`, `CursorNative`) in core.
      *Unchanged — verified all four stay registered in `Registry::builtin`.*
- [x] **7.2** Shrink embedded `models.json` to models for natively supported
      backends only. Everything else arrives via `register_provider` /
      `register_model`, or via an optional catalog extension that fetches and
      caches a registry (crush's pattern is the reference: tiny embedded
      fallback, ETag refresh, never block startup —
      `ref/crush/internal/config/catwalk.go`).
      *Landed as:* embedded catalog is now the three first-party providers
      Phi has auth flows for — anthropic (24), openai (41), cursor (11);
      76 models, ~40KB instead of 3,597 / 4.7MB. API gap closed on the way:
      `register_provider` was stored-but-dead (model resolution ran
      concurrently with runtime boot), so `build_session` now boots the
      runtime first, converts each `ProviderConfig` to registry models
      (`Registry::extended`, `session_builder::extension_provider_models`;
      api defaults to `openai-completions`), injects inline `api_key`s via
      `AuthStorage::set_runtime_api_key` (runtime-only, never persisted),
      and resolves models against the extended registry — so `--model
      my-router/my-model` works for extension providers in every mode.
      *Amended (bundled catalog):* shipping first-party-only broke every
      config pointing at a third-party model — "bring your own provider"
      is not a product. `phi-builtins` now ships `CatalogExtension`
      (`catalog.rs` + generated `catalog.json`, ~130 providers / ~3.3K
      models, ~700KB): xtask writes every models.dev provider a Phi
      backend can drive with plain API-key auth, and the builtin registers
      them through the same `register_provider` API user extensions use
      (last-write-wins, so a user extension can re-register a bundled
      name to point it at a proxy or attach a key). Providers needing
      account-specific endpoints or bespoke protocols (azure, vertex,
      bedrock, cohere, ...) are skipped and reported at generation time.
      `ProviderModelConfig` grew optional full-fidelity fields (cost,
      thinking_levels, input modalities, headers, adaptive_thinking) so
      the graft no longer degrades to defaults; the Lua path deserializes
      model tables via serde, one mechanism for all fields. One-shot mode
      now resolves models after runtime boot (it resolved against the
      base registry before — extension providers never worked there),
      `--list-models` and `/model` completion include the bundled
      catalog, and `find_model` distinguishes unknown-provider from
      unknown-model. Remaining gap: providers registered by user
      extensions still don't `/model`-complete (they register after the
      completion list is built). A fetch + ETag-refresh catalog extension
      (crush's catwalk pattern) remains optional future work.
- [x] **7.3** Retire or repurpose the `xtask` registry generator accordingly.
      *Repurposed:* generates only `EMBEDDED_PROVIDERS` (anthropic, openai)
      from models.dev, preserves the hand-maintained cursor-native entries
      across regenerations, and lost the `ai_gateway` module plus the dead
      per-provider classify branches.

### WS8 — Core robustness invariants (minimal ≠ flimsy)

- [x] **8.1 Project trust (do early).** Ship a builtin trust extension: first
      time Phi runs in a directory containing `.phi/` extensions/skills/prompt
      overrides, prompt the user; persist the decision. Until trusted, load
      nothing project-local. (pi reference: `project-trust.ts` in pi core;
      Phi hook: `check_project_trust`, `crates/phi-ext/src/runtime.rs:290`.)
      *Landed as:* `phi_session::trust` (persisted `TrustStore` +
      `has_project_content`), gating in `discover_extensions` /
      `config_extension_paths` / `discover_skills` /
      `discover_prompt_overrides` (`include_project` flag),
      `resolve_project_trust` in `session_builder` (stdin prompt in
      standalone TUI, untrusted-with-notice in mux/one-shot), and a
      `/trust [revoke]` builtin command via `UiAction::TrustProject`.
- [x] **8.2 Read-before-edit enforcement.** `edit_file`/`write_file` in the
      prelude refuse to touch a file the session hasn't read (or that changed
      on disk since the read — mtime/size already tracked by `file_ops.py`'s
      cache). Distinct, actionable error messages that teach the model the
      recovery path. (Reference: claude-code `FileEditTool`, error codes 6/7,
      including the "mtime advanced but content identical" false-positive guard.)
      *Landed as:* `_edit_precondition` in `file_ops.py`, gating `write_file`
      (existing files only) and `edit_file`'s old/new mode; the patch mode was
      already gated by hashline snapshot tags. The guard matches by content
      hash against the hashline snapshot store rather than the read cache's
      mtime/size, so the "mtime advanced but content identical" guard falls
      out for free, and the session's own writes count as reads. Two distinct
      errors: never-read → "call read_file(path) first, then retry"; stale →
      "re-read with read_file(path, fresh=True), rebase the change, then
      retry". `read_file(raw=True)` now records a snapshot so raw reads count.
- [x] **8.3 Subcall result spill.** Port upstream rlm's `SubagentResult`
      spill-to-tmp-file: subagent/`llm_query` results over ~50K chars get
      written to a tmp file; the REPL value carries a preview + path + full
      programmatic access. Phi currently caps REPL *stdout* (20K,
      `phi-rlm/src/prompts/constants.rs`) but embeds full child text in the
      subcall response (`phi-rlm/src/engine/subcalls.rs:242`). This is a
      regression vs upstream (`ref/rlm/rlm/environments/local_repl.py:62-189`).
      *Landed as:* `SubagentResult` in `phi-repl/src/router.py`
      (`SPILL_THRESHOLD = 50_000`): `llm_query` returns it for oversized
      text, `subagent` wraps the dict's `'text'` field. `str()`/`print`
      yields a 500-char preview + tmp path; `.full_result`, slicing, and
      delegated str methods keep full programmatic access (`__getattr__`
      delegation instead of upstream's hand-listed methods).
      `answer['content'] = r['text']` submits the full text, not the
      preview (`_capture_answer` unwraps). Spill behavior documented in
      `DEFAULT_REPL_TOOLS`. Below-threshold results stay plain `str`.
- [x] **8.4 Scaffold restoration.** Verify (and if absent, implement) that
      `router.py` force-restores reserved names (`llm_query`, `subagent`,
      `answer`, `context`, `bash`, …) after every exec so model code can't
      corrupt the environment (upstream: `_restore_scaffold`,
      `ref/rlm/rlm/environments/local_repl.py:664-696`).
      *Verified + closed the real gaps:* the function scaffold in
      `Env.globals` (llm_query, subagent, host fns, …) was already
      copy-protected by construction (each exec runs on `dict(globals)` and
      copy-back skips global names). The persistent holes were (a) prelude
      tools + context vars, which live in `locals` via `setup_code` and were
      freely overwritable, and (b) the `__builtins__` dict, shared by
      reference and mutable in place. Fix: a `_scaffold` snapshot of locals
      taken after prelude setup, reasserted (plus a fresh `__builtins__`
      copy) in `_restore_scaffold` after every exec; `answer` keeps its
      dedicated re-wrap logic.
- [x] **8.5 Aggregate truncation budget.** In addition to per-result caps, cap
      the *aggregate* tool/REPL output injected per iteration, so wide subagent
      batches can't blow the context. (Reference: claude-code
      `constants/toolLimits.ts` — 50K chars/result, 100K tokens/result, 200K
      chars aggregate per message.)
      *Landed as:* `clamp_aggregate` in `phi-rlm/src/llm_parse.rs`, applied
      in `run_loop` where tool results are banked into history. Budget =
      `max_output_chars × AGGREGATE_OUTPUT_FACTOR (5)` per iteration
      (mirrors claude-code's ~4-5× per-result:aggregate ratio). Overflow is
      truncated with an explicit `chars dropped` marker; fully elided
      results say so — never silent. UI events keep the full output.

### WS9 — Context compaction (core, part of the loop)

- [x] **9.1** In-loop auto-compaction in `phi-rlm`: trigger at a threshold of
      the model's context window *minus reserved headroom for the summary
      itself* (upstream rlm: 0.85 of window, `ref/rlm/rlm/core/rlm.py:600-642`;
      claude-code reserves ~13K + max-output tokens).
      *Landed as:* `engine/rlm/compaction.rs` — `maybe_compact` runs at the
      end of every `run_loop` iteration. Trigger: last completion's
      `input + cache_read + cache_write + output` ≥ `0.85 · window −
      20K headroom`. The window comes from a new
      `LanguageModel::context_window()` trait method (default `None` =
      compaction off; `ProviderLm` reads it from the registry `Model`).
      API gap closed on the way: the trait previously exposed no model
      metadata at all.
- [x] **9.2** Structured summary preserving: original intent, key technical
      concepts, files touched (with why), errors + user corrections, pending
      work, current state; keep recent N messages and all user messages
      verbatim. (Reference: claude-code `services/compact/prompt.ts` 8-section
      format.)
      *Landed as:* `COMPACTION_SUMMARY_PROMPT` (8 sections, incl. an
      RLM-specific "REPL variable names" section — env state is what makes
      an RLM summary resumable). Rebuilt history = system prompt + initial
      user prompt + summary + continue message + the most recent complete
      iteration verbatim (cut at a `Role::User` boundary so tool_use/
      tool_result pairs can't be orphaned). `[user interjection]` messages
      from the compacted span are preserved verbatim in the continue
      message. VCC-inspired addition (view over a lossless log): the
      compacted span is archived verbatim into the REPL as
      `compacted_{exchange}_{n}` before the in-band history is rebuilt, so
      the model can search/slice what the summary dropped — compaction
      demotes context to a cheaper view instead of deleting it.
      `SessionBeforeCompact` can veto or customize instructions;
      `SessionCompact` fires after.
- [x] **9.3** Compaction as an *additive marker* in the session JSONL — spliced
      into context on reconstruction, never destructive (opencode pattern;
      Phi's tree store already supports this shape). Wire the existing
      `SessionBeforeCompact`/`SessionCompact` events so extensions can observe
      or veto.
      *Landed as:* new `Line::Compaction(CompactionEntry)` record
      (append-only; unknown-type-tolerant readers in older builds skip it);
      engine emits `RlmEvent::Compaction`, the session TUI persists it via
      `append_compaction`. On reconstruction, `branch_restore` attaches
      summaries to their exchange (`RestoredExchange::compactions`) and
      `restore_session` splices them into the archived `history_N`.
      Message/repl records are never rewritten; the file-level `compact()`
      rewrite preserves the markers.
- [x] **9.4** Circuit breaker: stop auto-compacting after 3 consecutive
      failures (claude-code added this after telemetry showed runaway retry
      loops burning API quota).
      *Landed as:* `CompactionState.consecutive_failures` in `run_loop`;
      a failed or empty summary call advances it, success resets it, and at
      `MAX_COMPACTION_FAILURES (3)` compaction disarms for the rest of the
      run (history left untouched; the run proceeds until provider limits).
      Covered by `compaction_breaker_disarms_after_repeated_failures`.

### WS10 — Prove the API: reference extensions (landed)

Build these as extensions *because* they stress the API, in this order of value:

- [x] **10.1 MCP client extension.** Config: one shape covering stdio/sse/http,
      per-server tool allow/deny lists, shell-expanded env/headers (reference:
      `ref/crush/internal/config/config.go:182`). Exposes MCP tools via
      `register_tool` into the Python prelude namespace.
      *Landed as:* `McpExtension` (`phi-builtins/src/mcp/`) — a hand-rolled
      JSON-RPC client (stdio: tokio child process, newline-delimited,
      kill-on-drop, stderr tail in error reports; streamable http: reqwest
      POST with json-or-SSE responses, `Mcp-Session-Id` echo; legacy `sse`
      accepted in config but reported unsupported). Servers come from a
      crush-shaped `mcp` table in config.lua (`McpServerConfig` in
      `phi-session/config.rs`, the skills precedent) with a twist the skills
      field doesn't have: `Config::load_mcp` trust-gates the *project* layer
      specifically, because MCP servers spawn processes at startup. Env and
      header values shell-expand (`$VAR`/`${VAR}`/`$(cmd)`, empty-expanded
      headers omitted). Each tool registers as `ToolSpec { dispatch:
      Handler }` named `{server}_{tool}`, so all of WS12.3a applies unchanged
      — REPL stubs, per-server `expose = repl|native|both`, the 60s host
      watchdog, ToolCall/ToolResult interception; params derive from the
      server's JSON Schema (required-first for Python default ordering) and
      the schema passes through verbatim for native exposure. A
      `BeforeAgentStart` hook appends an `# MCP tools` roster so the model
      learns repl-exposed names (stubs land in the env silently). Connect is
      parallel across servers, bounded by per-server `timeout` (default 15s);
      failures degrade to warnings, never build errors. API gaps closed on
      the way: stub generation emitted invalid Python for params named after
      Python keywords or containing `-`/`.` (`python_param_name[s]` in
      phi-ext now sanitizes signatures while dict keys keep wire names) —
      MCP schemas are the first tool source core doesn't control. E2E test:
      fake python stdio server through connect → register → watchdogged
      invoke (`phi-builtins/tests/mcp_stdio.rs`). Remaining, deliberately:
      legacy SSE transport; OAuth for remote servers; dynamic
      `tools/list_changed` (the registry is immutable after build);
      per-session (not per-server) connection reuse in the mux.
- [x] **10.2 Approval/permission extension.** Uses the `ToolCall` interception
      event to gate `bash`/`write_file`/`edit_file` behind user confirmation
      with a persistable allowlist. Optional install — core stays pi-stance.
      *Landed as:* `ApprovalsExtension` (`phi-builtins/src/approvals.rs`),
      enabled by an `approvals = { enabled = true, rules = {...} }` config
      table (`Config::load_approvals` trust-gates the project layer like
      `mcp` — a cloned repo can't weaken the gate with its own allow rules).
      Ordered ruleset, opencode's last-match-wins: builtin defaults (ask on
      `repl`/`bash`/`write_file`/`edit_file`) → config rules → persisted
      "always allow" answers (`approvals.json`, keyed by canonical project
      path — the TrustStore layout) → session answers; unmatched calls
      proceed. Prompt options: Allow once / Always allow (persisted) / Deny;
      `/approvals` lists rules with provenance and `reset` clears saved
      answers. Asks fail **closed**: no UI, dismissal, or timeout all deny
      with an instructive tool error. Two API gaps closed to make it
      expressible (the Rule's corollary): (a)
      `EventHandlerRegistration.timeout` — a per-handler override of the
      dispatch watchdog budget (the 2s ToolCall default fails *open* on
      timeout, which an approval gate must never do; the watchdog itself
      remains mandatory, no unbounded option); (b) the ask-the-user
      primitive (pi's `ctx.ui.confirm` analog): `UiHandle`/`UiPrompt` in
      `phi-ext/src/ui.rs`, captured at register time via
      `ExtensionHost::ui_handle()` and late-bound by the host through
      `AppRuntime::connect_ui` — the session TUI services requests as a
      `SelectorKind::Prompt` selector (subject rendered into the transcript,
      requests queued FIFO, dismissed when the run ends), works mid-run in
      standalone and embedded/mux modes; one-shot `phi -p` never connects,
      so `has_ui()` is false and ask-rules deny. Lua parity shipped in the
      same change (WS4.6's lesson): `phi.on(event, handler, { timeout })`
      and `phi.ui.ask{...}`/`phi.ui.has_ui()`, with the Lua VM's
      instruction-hook deadline and the bridge wait both tracking the
      registration timeout; dogfood test ports the gate to Lua
      (`lua_ui_ask_gates_tool_call`). Honest remainders: under the
      REPL-only default the gate's granularity is the whole `repl` code
      blob (subject = the code) — per-tool-name granularity arrives with
      `coding_tools_native` or MCP tools; `ToolArgsOverride` is still
      unapplied by the engine, so edit-before-approve isn't expressible
      yet; dispatch stays fail-open on timeout for ordinary handlers (the
      gate protects itself via its raised budget).
- [x] **10.3 Structured subagent reports.** Extend the `subagent` return
      contract with a bounded summary field (jcode bounds completion reports to
      4K chars) — engine-side, but validated by an extension consuming it.
      *Landed as:* the subagent dict grew `'summary'` — always present,
      always ≤4,000 chars (`bound_summary` in `engine/helpers.rs`, jcode's
      4K bound with an explicit truncation marker, enforced host-side and
      never trusted to the child). Model-authored path: the child sets
      `answer['summary']` at any point — a *standing declaration* read from
      the live answer dict on every exec (`final_summary` on the REPL wire),
      so it survives a WS12.1 prose finish, not just `answer['ready']`;
      bounded at capture in `run_loop` and carried on the model-finish stop
      paths (answer/prose/default-answer; limit/cancel/failed stay `None`)
      through `Outcome` → `RlmCompletion.summary`. Fallback: a child that
      never set one gets `summary` derived from the bounded head of `text`,
      so the parent can always print `r['summary']` safely while `r['text']`
      may be spilled. The extension half (the item's acceptance):
      `EventPayload::AgentEnd` grew `summary` (serde-default, flows to Lua
      untouched) and `lua_agent_end_summary_reaches_extension` consumes it
      from a Lua handler; integration tests cover authored passthrough,
      prose-finish survival + derived fallback, and exact-4K truncation.
      Prompt docs updated: subagent dict shape in `DEFAULT_REPL_TOOLS` and
      an `answer['summary']` instruction on the answer bullet (jcode's
      report-instruction analog, shared prompt so children see it).

### WS11 — Mux client/server (canonical daemon, thin clients)

Goal: one canonical mux per user. A headless daemon owns all sessions and
screen state; any number of terminals attach as thin clients over a unix
socket. Advances the Mux differentiator (core work per the table above).

- [x] **11.1 Headless daemon + thin-client attach.** `phi --mux` spawns a
      detached `--mux-server` daemon if none is running, then attaches; every
      terminal (including the first) is an equal client. Sessions and in-flight
      agent turns survive any terminal closing; reattach resumes live.
      *Landed as:* `phi-mux/src/remote/{protocol,daemon,server,client}.rs`.
      Newline-JSON protocol on `$XDG_RUNTIME_DIR/phi/mux.sock` (0600, socat-
      debuggable); flock singleton + provably-stale-socket reaping + ready-fd
      spawn handshake (jcode's `socket.rs` pattern); per-connection tasks with
      an unconditional remove-on-drop registry (zellij's route-thread
      invariant); `MuxApp` refactored behind `InputSource`/`RenderSink`/
      `HostIo` seams so local single-process mode (`PHI_MUX_LOCAL=1`) and the
      daemon share one main loop. Tty concerns moved to the client (raw mode,
      OSC color probe, panic/signal restore) or became round-trips (`ctrl+g`
      `$EDITOR` runs on the last-active client's tty; copy = OSC 52 to that
      client; paste = client-side clipboard read). `phi --kill` terminates the
      daemon; `:quit` from any client does too.
- [x] **11.2 Per-client views (free navigation + independent sizing).** Split
      `MuxApp` into `MuxCore` (shared: workspace, session handles, watchers,
      persistence, clock) and `MuxView` (per client: focus, size, surface,
      renderer, sidebar scroll, command line, overlays, selection, note) so
      each client navigates and sizes independently while session content
      stays canonical. Decision recorded: a session's render size clamps to
      the **min among clients currently viewing it** (tmux grouped-session
      behavior), not the global min — clients on different sessions get full
      size; co-viewers get padding, never clipping (avoids compositing paths
      that don't exist today). Focus must move out of `Workspace`
      (`selected_project`, `project.selected`) into the view. Stage it:
      - [x] State split with a single view — behavior identical, local mode
            becomes "core + one view".
            *Focus extraction landed:* selection now lives in
            `workspace::Focus` (view state held by `MuxApp`), not
            `Workspace`; rescans preserve a view's selection via
            `refresh_project_with_focus` (mark-by-key → rebuild → restore),
            `build_sidebar_rows` takes `&Focus`, and the focus-steal
            regression tests drive the new API.
            *View struct landed:* `phi-mux/src/app/view.rs` defines
            `MuxView` holding all per-view state (focus, size, surface,
            renderer + its failure/invalidation counters, sidebar scroll,
            command line, archive viewer, active extension overlay,
            selection, note, alt-screen flag, cached scrollback);
            `MuxApp` = core fields + one `view: MuxView`. The N-view
            stage turns that field into a per-client map.
      - [x] N views: route input by sender, render per view via `send_to`,
            per-session size recompute (debounced) on focus change; host
            modals / edit requests route to the view(s) focused on the
            requesting session.
            *Landed as:* `MuxApp.views: BTreeMap<u64, MuxView>` (local mode
            = one view under the synthetic client id 0; the daemon seeds
            the first client's view from its consumed `Attached` handshake
            and creates/removes views on attach/detach). Input routes to
            the sender's view; every per-view method takes a `vid`. Each
            view composites its own surface/renderer at its own size and
            frames ship via `send_to(vid, Frame)` (stdout for local).
            Sizing: `session_target_pane` = min pane among views focused
            on a session, applied by `recompute_session_sizes` behind a
            150ms debounce (`pending_size_recompute` select arm);
            unviewed sessions keep their last size; no-op resizes are
            skipped. `ClientRegistry` lost `target_size`/`set_size` —
            `Resized` now carries `{id, cols, rows}` to the view. Routing:
            usage modal opens on every view focused on the requesting
            session; the model picker (single responder) and `$EDITOR`
            round-trips go to the last-active viewer, else any viewer,
            else an error reply; copy/paste OSC52 and clipboard reads
            target the acting client. Multi-view focus preservation:
            watcher/refresh paths use `refresh_project_with_focuses`
            (mark-by-key per view → rebuild → restore per view) and draft
            insert/remove wraps in `with_focus_preserved`; project removal
            shifts every view's `Focus`. Notes are per-view (`set_note_on`
            for the acting client, `set_note` broadcast for background
            events). Known shared state: scrollback offset lives on the
            vt100 screen, so co-viewers of one session scroll together.
- [x] **11.3 v1 gaps (deliberate, small follow-ups):** a later-attaching
      client's cwd doesn't join the sidebar (handshake already carries it);
      later clients don't re-theme (first client's OSC colors win — per-view
      palettes become possible after 11.2); per-invocation CLI overrides
      (`--model`) don't reach an already-running daemon; daemon sessions
      inherit the first spawner's environment.
      *Partial (WS11.3, 2026-07-04):* the cwd gap is closed — on attach,
      `RemoteEvent::Attached` now calls `workspace.add_project(info.cwd)`
      (no-op when the project already exists, so the first spawner isn't
      duplicated) and re-syncs watchers
      (`phi-mux/src/app/session.rs`).
      *Further (WS11.3, 2026-07-05):* the re-theme and `--model` gaps are
      closed. On attach, a client's `terminal_colors` re-derive the mux
      palette (`ScenePalette::from_extension_theme`/`from_terminal_colors`
      over the new colors — tmux-attach feel: the last client's colors win
      for the daemon, since the daemon renders one frame for all clients;
      per-view palettes would need per-view rendering). The `Attach`
      handshake grew an optional `model` field (`ClientMsg::Attach`,
      `AttachInfo`), and the client forwards its `--model` override; the
      daemon applies it to `shared.cli_args.model` so new session builds
      use the last attaching client's model. The remaining gap (daemon
      env inheritance — sessions inherit the first spawner's environment)
      is now closed: the `Attach` handshake grew a `shell` field
      (`ClientMsg::Attach`/`AttachInfo`), the client forwards its
      `$SHELL`/`$COMSPEC`, and the daemon applies it to
      `shared.shell` (seed + last-attach-wins, matching `--model`/re-theme).
      `env_info_block(cwd, shell)` reports the attaching client's shell in
      the `# Environment` prompt block instead of the daemon spawner's;
      `None` keeps the process-env fallback for standalone/one-shot/local.
      Covered by `env_info_block_uses_daemon_shell_override`.

### WS12 — Agent loop: coding-first RLM (latency + quality; queue-jump superseded by WS1)

Diagnosis (2026-07-03 audit of `run_loop` vs `ref/codex` + `ref/pi`): upstream
RLM is a **long-context-QA harness** (its own docs and training env are
chunk-a-huge-document QA), and Phi inherited three of its mechanics that
measurably hurt interactive coding. Codex stops on the model's finish signal
(`ref/codex` `core/src/session/turn.rs`, `end_turn`); pi stops the moment a
turn has no tool calls (`ref/pi` `packages/agent/src/agent-loop.ts:174`).
Phi does neither. This workstream does **not** soften differentiator #2 — the
REPL, recursion, and persistent env stay; what changes is the turn protocol
around them and the shape of the tool surface on top of them.

- [x] **12.1 Accept prose as the final answer (bug-level contradiction; do
      first).** `DEFAULT_MECHANICS` tells the model *"If you are finished …
      stop making tool calls and output your answer as text"*
      (`phi-rlm/src/prompts/constants.rs:21`), but `run_loop` refuses the
      first prose turn: it banks it, injects a contradicting `PROSE_NUDGE`
      ("use the repl tool or set `answer['ready']=True`") at 2 consecutive
      prose turns, and only accepts at `MAX_CONSECUTIVE_PROSE = 5`
      (`phi-rlm/src/engine/rlm/core.rs:45-53,931-1003`). A model that follows
      our own system prompt pays **up to 4 extra full-history completions at
      the end of every task**. Fix: a no-tool-call, non-empty prose turn is
      the final answer (`StopCause::Prose`), immediately. Keep
      `answer['ready']` and `<task_complete>` as explicit alternate paths;
      keep the nudge only for empty/whitespace-only responses. Side benefit:
      final answers stream to the user as text deltas instead of arriving as
      a Python string literal in `answer['content']`.
      *Landed as:* a no-tool-call, non-empty turn returns `StopCause::Prose`
      on the spot; `<task_complete>` and `answer['ready']` stay as the
      explicit alternate paths. `PROSE_NUDGE`/threshold/valve deleted; only
      genuinely *empty* turns (no tool calls, no text) get an
      `EMPTY_TURN_NUDGE` — nothing is banked (an empty assistant message is
      provider-hostile), and `MAX_CONSECUTIVE_EMPTY = 3` fails the run
      rather than accepting an empty answer or spinning. `DEFAULT_MECHANICS`
      now states outright that a tool-free turn ends the run. Prep commit
      removed the `ProsePolicy` strict/lenient machinery (strict tool-only
      mode contradicted this item and lenient was the only mode in use).
- [x] **12.2 Stop injecting a `Role::User` "Turn N:" message every iteration.**
      `build_user_prompt` pushes a user message before *every* model call
      (`core.rs:412-423`, `prompts/builder.rs:4-46`). No reference harness
      does this — codex/pi feed back tool results and let the model continue.
      It costs tokens, fragments the model into answer-each-turn mode, and is
      the exact consecutive-user-message hazard the `pending_steer` machinery
      exists to work around. Fix: turn-budget info moves to the system
      prompt; inject a reminder only when approaching a limit (codex's
      token-budget-reminder pattern, `turn.rs` `token_budget::maybe_record`).
      Also drop the turn-0 "Look at the context first; do not provide a
      final answer yet" gate for coding sessions — it forces a probe turn on
      questions that need none.
      *Landed as:* `build_user_prompt` deleted. `run_loop` pushes a user
      message only when the turn has real content: extension context
      injections, steer text from `save_point`, or a remaining-turns
      reminder once a capped run has ≤ `TURN_WARNING_THRESHOLD (3)` turns
      left (uncapped root runs get zero per-turn scaffolding). The cap
      itself is in the system prompt ("You have a hard cap of {n} turns" +
      the scarcity nudge); the one-time notes (extra contexts, prior
      histories, warm continuation) moved to `session_notes()`, appended
      once per send in `RlmSession::send`. The turn-0 probe gate is deleted
      unconditionally — its guidance already lives in `DEFAULT_MECHANICS` /
      `ORCHESTRATOR_ADDENDUM`. Knock-on fix: compaction's keep-last-
      iteration tail boundary re-anchored from last `Role::User` to last
      `Role::Assistant`, since iterations no longer carry user messages.
      Consecutive user-role messages are safe where they still occur — the
      providers merge them.
- [ ] **12.3 Tool surface through the extension API (API gap fix +
      evidence-gated experiment).** *(a) and (c) landed 2026-07-03; (b)'s
      builtin landed 2026-07-03 — only its measurement remains.* The tension: pure-CodeAct is
      off-distribution for models RL-trained on native read/edit/bash
      harnesses (a real quality risk), but the single-`repl` surface is also
      the differentiator's substance — composition (filter/loop/batch in one
      turn where tool-call harnesses burn a round-trip each), env-as-state,
      and the uniform tools-are-Python-functions extension story. Which
      effect dominates for coding with current models is **empirical, not
      doctrinal** — so don't decide it in core. Resolve via The Rule instead:
      - (a) *API gap fix:* `register_tool` today means only "inject a Python
        callable into the REPL namespace" (the WS10.1 shape). Grow the tool
        spec with a native exposure — `expose: repl | native | both` — so a
        registered tool can also surface as a provider `ToolDefinition`. The
        engine builds its per-turn tool list from the registry instead of
        hardcoding `repl_tool_definition()` (`core.rs:89-104,166-168`), and
        the exec loop generalizes from `repl`+`code` to per-tool dispatch
        (extension handlers get the doctrine-2 watchdog; loop detection
        generalizes its signature from code+stdout+stderr to args+result).
        Dispatch target for coding helpers is `python`: structured args map
        to the *same prelude functions in the same env*, so hashline cache,
        read-before-edit, and spill are shared regardless of surface — one
        implementation, two exposures (doctrine 5). The `repl` tool itself
        stays engine-registered (bare core must boot); the loop, guardrails,
        termination, and compaction stay engine-owned (doctrine 2 — the
        registry is the seam, the loop is the substrate). *Not gated on the
        (b) experiment* — start as soon as 12.1/12.2 land (same-file
        sequencing in `run_loop` is the only dependency); this is the API
        WS10.1/10.2 build on, and per the 2026-07-03 reorder it precedes the
        UI-side extension work.
        *Landed as:* `ToolSpec` grew `expose: ToolExposure`
        (`repl`/`native`/`both`, default `repl`), optional `input_schema`
        (JSON Schema; derived from `params` via `schema_from_params` when
        absent), and `dispatch: ToolDispatch` — `Handler(ToolHandler)` (the
        MCP/WS10.1 shape) or `Python` (implemented by a same-named function
        already in the REPL env; no stub generated, native calls map onto
        it). `register_repl_tool` became `register_tools`: per-turn tool
        list = engine-owned `repl` (bare core must boot) + every
        `native_tool_specs()` entry from the registry. The exec loop routes
        each streamed call (`ToolCallResult` now carries name + raw args):
        `repl` → `env.exec`; `Handler` → `AppRuntime::invoke_tool` (new,
        spawn_blocking + 60s watchdog — the same path the REPL-side
        `ext_tool` subcall now uses); `Python` → `__phi_native_call__(name,
        args_json)` executed in the env (JSON-encoded literals are valid
        Python literals, so args round-trip safely). ToolCall/ToolResult
        interception and ToolExecutionStart/End fire with the real tool
        name on every route; registry-tool executions emit
        `RlmEvent::ToolStart/ToolEnd`, repl keeps its CodeExec channel.
        Loop detection hashes name+args+stdout+stderr (`ExecutedBlock`
        lost `code`, gained `name`+`args`); unroutable calls still get a
        synthetic error tool_result so providers always see one result per
        tool_use. Lua `register_tool` accepts `expose` and `input_schema`.
        Bug found and fixed on the way: the generated prelude stubs call
        `__phi_ext_tool__`, which was never defined in the env — every
        repl-exposed extension tool died with a NameError; router.py now
        defines the shim (covered by `repl_stub_reaches_extension_handler`).
      - (b) *The hybrid ships as a default-off builtin* (`coding-tools-
        native`): registers bash/read_file/edit_file/write_file/grep/find/ls
        with `expose: both`. Enabling it by default is gated on measurement:
        after 12.1/12.2/12.4 land, run a fixed coding-task suite through
        oneshot mode, REPL-only vs hybrid, comparing success rate, turns,
        and wall-clock (12.6 telemetry is the instrumentation). No data, no
        default change.
        *Builtin landed as:* `CodingToolsExtension`
        (`phi-builtins/src/coding_tools.rs`), enabled by
        `coding_tools_native = true` in config.lua (`Settings` field,
        default off; registered in `build_runtime`, so one-shot runs get it
        too). All seven helpers register `expose: Both` + `dispatch:
        Python` with explicit input schemas (per-field descriptions the
        derived schema can't carry); no REPL stubs are generated — the
        prelude functions are the implementation, so hashline cache,
        read-before-edit, and spill hold on both surfaces. A drift test
        ties every native name to the `coding_tools_section()` roster.
        *Measurement harness landed (2026-07-04):* `bench/run.py` + a fixed
        8-task suite (`bench/tasks/`, objective out-of-workdir checkers,
        immutable-file hashing, reference solutions self-tested by
        `bench/validate.py`). Variants toggle `coding_tools_native` via an
        isolated per-variant `XDG_CONFIG_HOME` (auth/data dir untouched);
        metrics come from `--output ndjson` — the result line now includes
        `usage` (tokens/cost) alongside iterations/duration, and the runner
        folds the WS12.6 `IterationEnd {model, exec, dispatch}` split.
        Smoke-validated end-to-end (gemini-2.5-flash via openrouter: 4/4
        pass across both variants; native-exposure toggle probe-confirmed).
        Finding worth keeping: openrouter/qwen3-coder mangles tool-arg
        JSON server-side (Python-repr fragments arrive inside valid
        `code` strings), killing runs in *both* variants via loop
        detection — provider-side, not a phi bug (malformed-args handling
        verified: `unwrap_or_default` → synthetic error result). **The
        measurement run itself stays open**: full suite, `--reps ≥ 3`, on
        ≥ 2 models the user actually runs daily (needs the user's
        model/spend call) — no data, no default change.
      - (c) *REPL-preserving mitigations* worth doing regardless of the
        experiment's outcome: few-shot examples of idiomatic helper
        composition in the coding system prompt, and leaning on hashline
        `patch=` mode to cut the escaped-Python-string editing pain — the
        worst off-distribution surface.
        *Landed as (2026-07-03):* both mitigations live in
        `coding_tools_section()` (builtins-owned, next to the helpers they
        document — not in phi-rlm's identity). The `edit_file` bullet now
        leads with `patch=` as the preferred mode and documents the full op
        vocabulary inline (`SWAP N.=M:` + `+TEXT` rows, `DEL N.=M`,
        `INS.PRE/POST N:`, `INS.HEAD/TAIL:`; header copied from the view;
        all line numbers refer to the file as last read, no shifting;
        `r"""..."""` so payload rows are never escaped), demoting old/new
        to a fallback for short unambiguous substitutions. Three one-turn
        few-shot idioms appended: grep → filter in Python → targeted
        reads; a hashline patch mixing SWAP/INS.POST/DEL; loop-edit many
        files then `cargo check` in the same turn. The as-read-numbering
        doc claim is guarded by
        `hashline_multi_op_section_uses_as_read_line_numbers` in the
        prelude tests; the roster drift tests
        (`coding_roster_lists_expected_file_helpers`, tool_render's
        `specs_match_documented_helpers`) keep the rewritten bullets
        honest.
      Native exposure also unlocks per-tool truncation policies and
      provider-parallel tool calls for whoever enables it (pi parallelizes
      with a per-file mutation queue, `agent-loop.ts:373-388`; codex gates
      per-tool via RwLock, `tools/parallel.rs`). Previously listed as
      explicitly out of scope: abstracting the RLM *loop* itself behind an
      extension path. Per the 2026-07-03 "maximal extension surface"
      direction (see The Rule section), that rejection is downgraded from
      doctrine to sequencing — the loop stays engine-owned *for now* while
      the registry seam matures, but loop-as-extension is the eventual goal
      once the API can carry it without regressing the shipped defaults
      (now scheduled: WS1 carries it as agent packs).
- [x] **12.4 Coding-session prompt retune.** The "You are a Recursive Language
      Model" identity and the `ORCHESTRATOR_ADDENDUM` ("if you find yourself
      about to write several sentences of unaided reasoning, that is usually
      a sign to delegate with `subagent`", `constants.rs:23`) are tuned for
      document QA; for coding they multiply latency (each `subagent` is a
      full child RLM loop) and degrade output (children lack context; the
      documented `bg(subagent(...))` deadlock footgun lives in the system
      prompt). Fix: coding sessions get a coding-agent identity; delegation
      guidance becomes claude-code-shaped — subagents for fan-out *search*
      and isolating huge outputs, not the default reasoning path. Keep the
      orchestrator addendum for genuine long-context/QA payloads.
      *Landed as:* `phi_rlm::CODING_IDENTITY` (claude-code-shaped: do the
      work yourself in the REPL; `subagent` for fan-out search and isolating
      oversized output; `llm_query` for one-shot extraction; never delegate
      ordinary reasoning). The interactive TUI and plain `phi -p` runs set
      it via `custom_system_prompt` (a `.phi/SYSTEM.md` override still wins)
      with `orchestrator: false`; `phi -p --context <file>` keeps the QA
      identity + orchestrator addendum — that's the long-context payload
      path the addendum was tuned for. The QA delegation stance ("use
      subagent as your default delegation path") moved out of the shared
      `DEFAULT_REPL_TOOLS` listing (it contradicted whichever identity was
      active; the addendum already carries it for QA runs). Both hosts'
      `user_prologue` ("You are a coding agent…" as a *user* message) died —
      the system prompt owns the identity now, and `user_prologue` reverts
      to its real job: the `preamble=` channel for subagent children.
- [x] **12.5 Retry transient provider errors with backoff.** `run_loop`
      returns `Outcome::failed` on the first `complete_root` error
      (`core.rs:643-657`) — a single 429/529 blip kills a long run. Both
      references retry: codex exponential backoff `200ms·2ⁿ` with jitter
      (`responses_retry.rs`, `util.rs:85-90`), pi two layers (provider
      settings + app-level `baseDelayMs·2ⁿ`, default 3 retries). Verify what
      `phi-ai` does internally; add the missing layer where it belongs.
      *Verified already implemented (2026-07-03)* — no missing layer.
      Engine seam: `ProviderLm::complete_streaming`
      (`phi-rlm/src/provider.rs:191-241`, commit c5f31fc) retries transient
      failures (429/5xx/529, overload, timeout, transport) with
      `500ms·2ⁿ ±20%` jitter, 3 retries, only while nothing has streamed
      (retrying after visible deltas would duplicate output), with
      terminal-quota 429s fast-failed via `is_terminal_rate_limit`.
      Transport layer: phi-ai adds mid-stream retry with rollback and a
      fresh unpooled client (commit 1171000). `run_loop`'s error arm is now
      only reached after both layers are exhausted — correct.
- [x] **12.6 Turn telemetry before further tuning.** `IterationEnd` already
      carries a duration; split it into model-time vs exec-time vs
      event-dispatch per iteration so "slow" is attributable. Candidate
      follow-ups that should wait for this data: parallel execution of
      read-only tool calls, prompt-cache-hit verification (history is
      append-only between compactions, so provider prefix caching *should*
      already work — confirm, don't assume).
      *Landed as:* `RlmEvent::IterationEnd` grew `model` (root completion
      request + stream), `exec` (sum of the iteration's REPL block
      durations), and `dispatch` (extension hooks: context injections,
      tool_call/tool_result interception, execution start/end) alongside
      the wall `duration` — all serialized as seconds in NDJSON. Raw
      attributions, deliberately not an additive breakdown: exec overlaps
      model because tool calls execute eagerly during the stream. The
      listed follow-ups remain open pending collected data.

What the audit confirmed is *right* in the loop (keep, don't churn): loop
detection with output-aware signatures, compaction archiving the compacted
span into a REPL variable, hashline read-dedup stubs, exec-during-stream,
timeout-to-background-job conversion, and steer-text merging. The problem is
the turn protocol, not the engineering.

---

## Non-goals (rejected explicitly — don't relitigate without a new decision)

- **OS-level sandboxing in core** (codex-style landlock/seatbelt). Considered;
  rejected in favor of pi's stance. Revisit only if Phi's positioning changes.
- **Declarative hooks config in core.** The Lua event surface (~25 events,
  `crates/phi-event/src/lib.rs`) *is* Phi's hooks system. A "shell hooks"
  convenience can be a builtin extension later if demand appears.
- **Embedded mega model catalog.** See WS7.2.
- **Feature parity with claude-code/codex.** Phi competes on mux + RLM +
  extensibility, not breadth. *Presentation* parity (WS2 style packs) and
  *shape* parity (WS1 agent packs — prompt, tool surface, turn protocol as
  configuration) are in scope precisely because they exercise extensibility —
  the line is: imitate the look and the shape through the extension API,
  never import the feature set into core or the shipped defaults. A codex
  pack that exists because the API can express it is on-plan; a codex
  feature hardwired because a pack couldn't express it is the exact failure
  the Rule exists to reject.

## Reference map (all under `ref/`, git-ignored checkouts)

| Ref | What to steal | Where |
|---|---|---|
| `pi` | Philosophy, extension API shape, session tree, keybinding discipline, trust | `packages/coding-agent/docs/extensions.md`, `docs/session-format.md`, `CONTRIBUTING.md` |
| `rlm` | Upstream engine: spill, scaffold restore, compaction threshold, remaining-budget propagation | `rlm/environments/local_repl.py`, `rlm/core/rlm.py`, `docs/architecture.md` |
| `claude-code` | Tool invariants (read-before-edit, truncation budgets), compaction summary format, circuit breakers | `tools/FileEditTool/`, `constants/toolLimits.ts`, `services/compact/` |
| `codex` | Rust patterns if ever needed: protocol/core/frontend crate split, approval-carries-policy-amendment | `codex-rs/protocol/`, `core/src/tools/orchestrator.rs` |
| `crush` | MCP config shape, self-documenting config schema, model roles, registry fallback pattern | `internal/config/config.go`, `internal/config/catwalk.go` |
| `opencode` | Additive compaction markers, permission rule semantics (last-match-wins) | `src/session/compaction.ts`, `src/permission/index.ts` |
| `oh-my-pi` | Hashline/anchor edits (Phi has this), typed subagent contracts, what "maximal pi" looks like — a cautionary + idea mine | `README.md` |
| `jcode` | Bounded subagent completion reports, compaction thresholds incl. payload-byte budget; daemon lifecycle (flock lock, stale-socket reaping, ready-fd handshake — stolen for WS11.1) | `jcode-swarm-core/src/lib.rs`, `jcode-compaction-core/src/lib.rs`, `crates/jcode-app-core/src/server/socket.rs` |
| `zellij` | Canonical-row scrollback reflow on resize; client/server minimal core (dumb client, per-client fanout, remove-client-on-any-exit invariant — stolen for WS11.1) | `zellij-server/src/panes/grid.rs`, `zellij-utils/src/ipc.rs`, `zellij-server/src/route.rs` |

## Verification

Per `AGENTS.md`: build and test through Nix, not cargo.

```bash
nix build .#checks.x86_64-linux.workspace-test --print-build-logs
nix build .#phi-app
nix run
```

Definition of done for the plan overall:

1. The Rule is in `AGENTS.md` and `phi-builtins` is the largest consumer of the
   extension API.
2. A bare build without `phi-builtins` still runs (engine + REPL + shells) —
   enforced by a flake check (WS4.5), not by prose.
3. Project-local extensions load only after explicit trust.
4. The prelude enforces read-before-edit and spills oversized subcall results.
5. Long sessions auto-compact without losing user messages.
6. MCP works — as an extension nobody had to modify core for.
7. A user Lua extension can reimplement a builtin command (including its
   `UiAction`s) and a builtin overlay without touching core (WS4.6) — the
   extension API the Rule dogfoods is the one users actually get.
8. A completed task ends on the model's first prose turn — zero wasted
   end-of-task completions — and no `Role::User` scaffolding is injected on
   ordinary iterations (WS12.1/12.2). Transient provider errors never kill a
   run (WS12.5).
9. `/style claude-code` (or `codex`, `crush`) re-skins a running session to
   be hard to distinguish from the real harness at a glance — with every
   pack a pure Lua extension and zero pack-specific code in core (WS2).
10. `/agent pi` (or `codex`) swaps the entire agent — prompt, tool surface,
    turn protocol, env — as a pure Lua pack with zero pack-specific code in
    core; the pi pack's request stream carries no `repl` tool; the RLM
    engine ships as the default pack rather than a hardwired privilege; and
    a user extension, on the public API alone, can wire agents that spawn
    and message each other or route a question to a peer instead of the
    human (WS1).
