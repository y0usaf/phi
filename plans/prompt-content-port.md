# Phi: port claude-code's system-prompt *content* into `build_system_prompt`

> Source of the assessment: an adversarially-verified cross-codebase read of `~/Dev/claude-code/constants/prompts.ts` (the `getSystemPrompt` section-array + `SYSTEM_PROMPT_DYNAMIC_BOUNDARY` cache framework) against Phi's `crates/phi-rlm/src/prompts/` (fixed-order `String` builder) and `crates/phi-runtime/src/runtime.rs` (`filter_system_prompt` / `BeforeAgentStart` hook). Three skeptics (tool-protocol, caching-architecture, identity/RLM-model) confirmed the central verdict and caught the corrections folded in below. **Mine for content; do not import the structure.**

> **Line-anchor caveat (same as `plans/phi-durable-memory.md`):** anchors below were checked against current source on 2026-06-20 and are correct *as of that date*. **Before editing any cited line, re-locate the symbol by name (grep) — do not trust the number.** Every named function/struct/const exists and behaves as described.

## Outcome in one paragraph

Add ~8 new optional content sections to `SystemPromptOptions`, slot them into the existing fixed-order `build_system_prompt`, and rewrite every "tool use / tool call / call the X tool" phrase to "```` ```repl ```` block / call the REPL helper X". **Do not** adopt claude-code's section-array-with-boundary assembler, its section-cache registry, or its cacheScope tiers — they solve problems Phi doesn't have (native `tool_use`, multi-block cache control, cross-org global cache scope, the 2^N-variant hashing from runtime conditionals in the static prefix). Phi already has the structural split that machinery exists to produce: `build_system_prompt` = stable prefix, `filter_system_prompt` = dynamic tail. Gate the coding-flavored sections on `user_prologue` (coding-agent mode), not default-on for every identity. Reconcile the pre-existing OAuth dual-identity first.

---

## The five locked decisions

1. **Keep the fixed-order `String` builder; do not become a section-array assembler.** `build_system_prompt` (`builder.rs:144`) pushes an ordered `Vec<String>` joined by `"\n\n"`. A `SYSTEM_PROMPT_DYNAMIC_BOUNDARY`-style marker inside one concatenated `String` is meaningless — `cache_control` is per-Anthropic-block (`anthropic.rs:107-111`), not per-in-string-marker. Realising any boundary value would require splitting static-vs-dynamic into *separate Anthropic system blocks* and carrying them separately through `Context` — a provider/engine refactor, not a prompt change. **Rejected:** the boundary + `systemPromptSection`/`DANGEROUS_uncachedSystemPromptSection` registry + three cacheScope tiers. Phi has no 2^N problem (its static prefix carries no runtime conditionals; all dynamic content lives in the hook *after* `build_system_prompt`) and no access to global cache scope (Anthropic firstParty-only; Phi is multi-provider). *(Rejected alternative reconsidered and dropped — see Decision 5 for the one forward cache option.)*

2. **Rewrite all native-tool framing to repl-fence framing.** Phi has no native provider `tool_use`: `Context` has no `tools` field (`phi-ai/types.rs`), `build_body` emits no `tools`/`tool_choice`/`tool_result` array (`anthropic.rs:91-127`), tools are REPL helper functions described as prose and invoked via ```` ```repl ```` fences. Every ported sentence that says "tool use" / "tool calls" / "call the X tool" / "parallel tool calls" is rewritten to "```` ```repl ```` block" / "call the REPL helper X" / "one ```` ```repl ```` block with multiple statements (or back-to-back blocks)". This is non-negotiable and is why several "direct ports" are actually *port-with-adaptation* (see Track 1).

3. **Coding-agent content is gated on `user_prologue`, not default-on.** `DEFAULT_IDENTITY` (`constants.rs:1-4`) is a general-purpose RLM, not a coding agent; the coding-agent framing lives only in `user_prologue` (`session_builder.rs:127`, a *user* message), emitted only on the CLI coding path (`engine.rs:790`). `tests.rs:64-80` (`system_prompt_custom_identity_does_not_drop_tools`) guards **tool retention** under a custom identity — not identity *coherence*. So `doing_tasks`/`tone_and_style`/`actions` default to **off** for the bare-RLM identity and **on only when `user_prologue` is set** (coding-agent mode). `env_info`/`output_efficiency`/`cyber_risk`/`system_reminder_semantics`/`summarize_tool_results` are identity-neutral and may default on. *(Rejected: "optional-but-default-on for everything" — contradicts the bare-RLM identity and ~doubles the prompt every turn with no cache benefit to amortise it — over-prompting risk against `DEFAULT_MECHANICS`' "Your own context window is small".)*

4. **Reconcile the pre-existing OAuth dual-identity before stacking more content.** On every OAuth request, `build_body` prepends `CLAUDE_CODE_IDENTITY` ("You are Claude Code, Anthropic's official CLI for Claude.") as an **uncached** first system block (`anthropic.rs:101-104`), then pushes `ctx.system_prompt` (which *is* `DEFAULT_IDENTITY`, the RLM identity) as the cached block (`:106-112`). The model therefore already receives two contradictory identities today. Porting doing-tasks/intro content without resolving this stacks a third layer. Resolve first (Track 0).

5. **One forward cache option, deferred and gated.** If future measurement shows cache-hit churn from the hook rewriting the single cached block every content-changing send, the cheapest win is a **second uncached Anthropic system block** for the dynamic tail: route `build_system_prompt` output (stable) into the cached block, `filter_system_prompt` output (dynamic) into a second uncached block appended after it. Phi already emits a `Vec` of system blocks in OAuth mode (`anthropic.rs:100-115`), so the *provider* change is small; the real work is an engine/`Context` refactor to carry static-vs-dynamic separately. **Not part of this plan.** Documented here so the deferral is explicit, not silent. *(Note: today's cache is less dire than it sounds — `build_system_prompt`'s deterministic rebuild does not itself bust the cache; only a hook mutation on a send where extension content actually changes does, and no-extension sends cache-hit.)*

---

## Track 0 — Reconcile the OAuth dual-identity (prerequisite, M)

The pre-existing stack, on the OAuth path (`Auth::OAuthToken`):
- system block #1 = `CLAUDE_CODE_IDENTITY` (uncached) — "You are Claude Code…"
- system block #2 = `ctx.system_prompt` (cached) = `DEFAULT_IDENTITY` — "You are an RLM attached to a Python REPL…"

On the API-key path, only block #2 is sent. So the model is "Claude Code" *and* "an RLM" only over OAuth.

Two acceptable resolutions (pick one before Track 1; record the choice here):
- **(a) Keep `CLAUDE_CODE_IDENTITY` as a thin OAuth compliance shim and make the RLM identity the substantive one.** Leave the OAuth block as-is (the API requires it to validate the token — `anthropic.rs:102-103` comment), but treat it as a *carrier/header*, not the agent identity. Don't port claude-code's "interactive agent that helps with software-engineering tasks" intro anywhere that would reinforce it; instead fold any coding-agent identity into `user_prologue` (Decision 3) so it lives in the user-message channel, not the system blocks. Lowest-risk, no API-policy change.
- **(b) Make the OAuth identity block the legit home for the claude-code-style framing and slim the cached block to RLM-mechanics-only.** Riskier — requires the cached `ctx.system_prompt` to drop the "You are…" opener in favour of a mechanics-only lead, and depends on whether the OAuth gate tolerates identity text living *outside* the first block.

**Recommendation: (a).** Keep `CLAUDE_CODE_IDENTITY` as an OAuth-only carrier; never reinforce it in ported content; let `user_prologue` own the coding-agent identity. **Verify by name:** `CLAUDE_CODE_IDENTITY` (`anthropic.rs:17`), the `is_oauth` push (`anthropic.rs:101-105`), `build_body` system assembly (`anthropic.rs:100-115`).

Acceptance: a test asserting the non-OAuth path emits exactly one system block and the OAuth path emits the shim + the cached RLM block in that order; and that no ported section duplicates the "interactive agent"/"Claude Code" identity.

---

## Track 1 — Port the content sections (the main work, M–L)

### Slot order (final)

Extends the existing order (`builder.rs:137-138`) — new fields bolded:

> identity → **cyber_risk (inline after identity, or its own field)** → core_tools → custom_tools → **doing_tasks** → **actions** → mechanics(+ **system_reminder_semantics** folded in) → **tone_and_style** → **output_efficiency** → [orchestrator] → [turn_scarcity] → project_context → available_skills → extra_guidelines → **env_info** → cwd → **summarize_tool_results** → append

`extra_guidelines` is kept where it is (it's the existing extension-free bullet mechanism; don't move it). Each new field is `Option<&str>` defaulting to `None` = not emitted, matching the existing skip-when-empty pattern (`custom_tools`, `orchestrator`, `turn_scarcity`).

### 1a. New `SystemPromptOptions` fields (S)

Add to the struct (`builder.rs:76-123`):
- `cyber_risk: Option<&'a str>`
- `doing_tasks: Option<&'a str>` *(gated, Decision 3)*
- `actions: Option<&'a str>` *(gated)*
- `tone_and_style: Option<&'a str>` *(gated)*
- `output_efficiency: Option<&'a str>`
- `env_info: Option<&'a str>` *(caller-supplied, like `cwd`)*
- `summarize_tool_results: Option<&'a str>`
- `system_reminder_semantics: Option<&'a str>` *(folded into `mechanics` block, or its own small field early — see 1c)*

Compose each in `build_system_prompt` in the slot order above, every one guarded `if let Some(x) = opts.x && !x.is_empty()`. Add corresponding `DEFAULT_*` consts in `constants.rs` for the "default-on" ones (Decision 3) so they're on unless overridden/empty.

### 1b. The five clean (verbatim-ish) ports

Identity-neutral, no tool-roster dependency — port nearly verbatim from `~/Dev/claude-code/constants/prompts.ts`:

1. **`# Environment` block** ← `computeSimpleEnvInfo` (`prompts.ts:651`). cwd, git-repo, platform, shell, OS version. **Drop** the model-marketing-name / knowledge-cutoff / fast-mode lines (Anthropic-model-family-specific; Phi is multi-provider and the cyber-risk/undercover suppression is moot). Compute once per session, not per turn (Decision 5 cache note). Lands as `env_info`, before `cwd`. Phi already emits a bare `Working directory:` line (`builder.rs:251`) — keep `cwd`'s existing line, add the structured block before it.
2. **Output efficiency** ← `getOutputEfficiencySection` external variant (`prompts.ts:403`). "Go straight to the point, simplest approach first, lead with the answer, skip filler, focus on decisions/status/blockers." Pure prose. **Skip** the ant-only `# Communicating with the user` variant. Default-on.
3. **Cyber-risk instruction** ← `CYBER_RISK_INSTRUCTION` (`claude-code/constants/cyberRiskInstruction.ts`). Dual-use security boundary (authorized pentesting/CTF/defensive OK; destructive/DoS/mass-targeting/supply-chain/evasion refused; dual-use needs authorization context). **Verbatim const** — Phi has no equivalent guardrail today. Default-on. *(Note the file's own "DO NOT MODIFY WITHOUT SAFEGUARDS REVIEW" header — port the text unchanged.)*
4. **`<system-reminder>` tag semantics** ← `getSystemRemindersSection` (`prompts.ts:131`). Phi *uses* the tag — `PROSE_NUDGE` is one (`constants.rs:107`) — but never *defines* what it means to the model. Port the definition so the model treats `PROSE_NUDGE` and any future injections correctly. Pure tag-semantics prose; port verbatim. Default-on.
5. **`# Executing actions with care`** ← `getActionsSection` (`prompts.ts:255`). Reversibility/blast-radius: freely take local reversible actions; confirm before hard-to-reverse / shared-state / destructive ops (force-push, `rm -rf`, PRs, messages); root-cause over `--no-verify` shortcuts; authorization is scope-limited. Maps onto Phi's `bash()` helper. **Gated** (coding-mode): the "confirm before destructive ops" presumes an interactive confirmation channel; keep it in coding-agent mode where that channel exists.

### 1c. The three port-with-adaptation sections (rewrite for repl fences — Decision 2)

These carry native-tool framing the clean ports don't; the *substance* ports, the *wording* must change:

6. **`# Doing tasks`** ← `getSimpleDoingTasksSection` (`prompts.ts:199`). Keep the principle-level bullets (no speculative abstractions / error-handling / comments, read-before-modify, no time estimates, OWASP, faithful reporting). **Strip** `ASK_USER_QUESTION_TOOL_NAME`, `MACRO.ISSUES_EXPLAINER`, and all `USER_TYPE==='ant'` sub-bullets. **Rewrite** any "use the Read/Edit tool" → "use the `read_file`/`edit_file` REPL helper". **Generate tool names from `coding_tools_section()`** (see Track 3) to prevent drift. Gated (coding-mode).
7. **`# Tone and style`** ← `getSimpleToneAndStyleSection` (`prompts.ts:430`). The no-emoji / `file_path:line_number` / `owner/repo#123` bullets port verbatim. **Rewrite** the "no colon before tool calls" bullet → "no colon before a ```` ```repl ```` block", and "tool calls may not be shown directly" → "```` ```repl ```` blocks may not be shown directly". Gated (coding-mode).
8. **Summarize tool results** ← `SUMMARIZE_TOOL_RESULTS_SECTION` (`prompts.ts:841`). One-liner: persist important info from results because they may be cleared. **Rewrite** "tool results" → "```` ```repl ```` block output and `rlm_query` results" (Phi's vocabulary — repl outputs return as user messages; `DEFAULT_MECHANICS` says "REPL outputs", not "tool results"). Pair with the `frc` reword (1d). Default-on.

### 1d. The rework-don't-port sections (different mechanism)

9. **`# Using your REPL helpers`** ← `getUsingYourToolsSection` (`prompts.ts:269`). Full rewrite, not a port. "Prefer the dedicated REPL helpers (`read_file`, `write_file`, `edit_file`, `grep`, `find`) over raw `bash()` for file operations — dedicated helpers let the user review your work. You may issue several independent pieces in one ```` ```repl ```` block or in back-to-back blocks (parallelism). For multi-step subtasks delegate with `rlm_query` / `rlm_query_batched`" — cross-ref `ORCHESTRATOR_ADDENDUM` (`constants.rs:21`). **Drop** the Task/TodoWrite tracking (Phi has no task tool) and the AgentTool subagent framing (Phi's recursion primitive is `rlm_query`, already covered by the orchestrator addendum). Generate tool names from `coding_tools_section()` (Track 3). Gated (coding-mode).
10. **`frc` / function-result-clearing** ← `getFunctionResultClearingSection` (`prompts.ts:821`). Rewrite for Phi's *durable-execution* model, not Anthropic cached-microcompact: "Old ```` ```repl ```` execution output and `rlm_query` results may be cleared from your message history to free space; the N most recent are kept" + the durable skip-on-replay / first-emit-wins semantics already in the engine. **Gate on Phi's own durable/compact activation**, not a provider model pattern. Pair with `summarize_tool_results` (8). Default-on if durable/compact active; else off.
11. **`getHooksSection` + `getSimpleSystemSection`** ← `prompts.ts:127`,`:186`. **Strip** hooks-as-user-feedback and permission-mode-on-denied-tool framing (Phi has no `settings.json` hooks, no permission tool). **Keep and rewrite** only the universally-useful piece: "All text you output outside ```` ```repl ```` blocks is shown to the user" (replacing "outside tool use"). The automatic-summarization bullet → Phi's compaction, reworded. Gated (coding-mode) for the system-rendering bullets; the compaction note default-on.
12. **`getSimpleIntroSection` (identity branch)** ← `prompts.ts:175`. **Do NOT merge** with `DEFAULT_IDENTITY`. Keep the RLM identity as the first section (Decision 4). Port only the URL-guessing restriction ("NEVER generate or guess URLs unless confident they're for helping with programming") as a subsequent bullet. **Drop** the outputStyleConfig branch (no output styles). Default-on for the URL restriction.

### 1e. Explicitly rejected (do not port)

- `SYSTEM_PROMPT_DYNAMIC_BOUNDARY` + `splitSysPromptPrefix` + `cacheScope` tiers + `shouldUseGlobalCacheScope` + `prompt_caching_scope` beta header — Decision 1.
- All `USER_TYPE==='ant'` gating: `numeric_length_anchors`, `ant_model_override`, the proactive short-circuit path, undercover model-name suppression, ant repo-classification cache-clear — Anthropic-internal; dead/misleading in Phi.
- `getScratchpadInstructions`, `getBriefSection`/`getProactiveSection`/`<tick>` keep-alive, `numeric_length_anchors`, token-budget — no Phi equivalent; Phi's root loop is `engine.rs` `run_loop` + answer-ready, not an autonomous keep-alive.
- `getDiscoverSkillsGuidance` + DiscoverSkills — Phi has its own `<available_skills>` path (`builder.rs:155-176`); do not import claude-code's separate skill-discovery runtime.
- The `OutputStyleConfig` plugin / priority-merge system — if Phi wants per-session voice control, use `SYSTEM.md` overrides (`phi-session/src/prompts.rs:discover_prompt_overrides`), not the plugin machinery. `keepCodingInstructions` maps to the existing custom-identity-drops-tools guard (`tests.rs:64-80`).
- `mcp_instructions` — MCP is deferred (memory: "MCP deferred"); route future dynamic tool-availability through the existing `SystemPromptMod` hook (`runtime.rs:81`), not the `DANGEROUS_uncachedSystemPromptSection` mechanism.

---

## Track 2 — Wire the gate + the env computation (S–M)

- **The `user_prologue` gate.** In `engine.rs:813-815` where `SystemPromptOptions` is built, set the gated fields (`doing_tasks`, `actions`, `tone_and_style`, the system-rendering part of hooks) from their `DEFAULT_*` consts **iff `self.config.user_prologue.is_some()`**; else leave `None`. The identity-neutral fields (`cyber_risk`, `output_efficiency`, `system_reminder_semantics`, `env_info`, `summarize_tool_results`) are set unconditionally. `user_prologue` is currently cleared after the cold root at `engine.rs:371 — check whether gated content should persist across warm turns (it lives in the system prompt, rebuilt each send, so re-check the gate each build, not once).
- **`env_info` computation.** Add a session-time computation (cwd, is-git, platform, shell, OS version) mirroring `computeSimpleEnvInfo`. Phi likely already has pieces (the `Working directory:` line); consolidate. **Memoize per session** (Decision 5) — recompute only on cwd change / worktree enter-exit, not per turn. Keep provider-specific bits behind the Anthropic check if any creep in.
- **Wire `context_files` / `skills` while here?** *Out of scope but flagged:* these struct fields exist and are tested (`builder.rs:115-116`, `tests.rs:83, :99`) but the only runtime caller leaves them `..Default::default()` (`engine.rs:815`) — **dead in the real prompt path**. The slot order above includes them, but they don't fire today. Wiring AGENTS.md/CLAUDE.md discovery + SKILL.md frontmatter into `context_files`/`skills` is a separate prerequisite if you want those slots to mean anything; note it but don't block this plan on it.

Acceptance: a test asserting a bare-RLM build (no `user_prologue`) emits `cyber_risk`/`output_efficiency`/`env_info` but **not** `doing_tasks`/`actions`/`tone_and_style`, and a coding-mode build (`user_prologue` set) emits all of them.

---

## Track 3 — Kill the tool-name drift risk (S, but load-bearing)

`coding_tools_section()` (`phi-builtins/src/prelude/mod.rs:15`) is **hand-written prose** (`bash`, `read_file`, `write_file`, `edit_file`, `grep`, `find`), separate from `CODING_PRELUDE` (`:7-13` = `include_str!` of the real `.py` files). There is no generation step tying the prompt's tool names to the prelude's actual function signatures — so ported doing-tasks/using-helpers text could name tools that don't exist or miss new ones, with no test catching it.

- **Minimum:** add a test (in `phi-builtins` or `phi-rlm/tests.rs`) asserting every tool name mentioned in the ported doing-tasks / using-helpers / system sections is present in the registered prelude functions (`CODING_PRELUDE`-derived set), and vice versa for the ones the prompt *should* name. Parse names out of the prompt text with a simple regex over the ```` ``` ````-fence roster style.
- **Better (stretch):** generate the using-helpers / doing-tasks tool-usage sentences from a single `ToolSpec`/prelude manifest (`phi-ext` `ToolSpec` already exists — `runtime.rs:10`) so the prompt and the prelude can't drift. Decide per-section whether prose generation is worth it.

Acceptance: the drift test exists and passes against the ported text.

---

## Track 4 — Extension-hook sanity (S, defensive)

`filter_system_prompt` (`runtime.rs:64-91`) runs on the **cold root** (`engine.rs:794`) **and every warm send** (`engine.rs:1725`), applying `SystemPromptMod { prepend, append, identity: _ }`. Two pre-existing behaviours the larger prompt makes more expensive — *not introduced* by this port, but worth pinning down:

- **`identity: _` is discarded** (`runtime.rs:81`): `IdentityInjection` is plumbed through `phi-ext` and a `DurableEntry::IdentityInjected` ledger variant exists, but no path writes the ledger entry. If a ported section ever becomes the home for injected identity, this half-wired durable-replay channel becomes dead code. **Decide now:** connect it or remove the dead variant. *(Out of scope to fix here; flagged so the port doesn't quietly widen the gap.)*
- **Per-send re-injection, not unbounded growth.** Verified: `initial_history` rebuilds the system message fresh each warm send, so hook appends **do not accumulate** across turns — the transcript stores only user/assistant messages. The residual cost is the hook re-running per send (re-paying the append + re-busting the cache when content changes). Larger static sections make each cache bust *more* expensive, not more frequent. No fix required; document as accepted.

Acceptance: a test asserting a repeated `filter_system_prompt` call on a fresh-built system prompt does not grow the string across N turns (regression guard for the "unbounded growth" worry, which is currently false and should stay false).

---

## Identity/RLM-model compatibility — the ordering constraint

The sharpest finding: `DEFAULT_MECHANICS` (`constants.rs:15-19`) says "Do not flip `answer["ready"] = True` on turn 1 without first inspecting `context`" and "Plan in prose, then write your ```` ```repl ```` blocks". `getOutputEfficiencySection` says "lead with the answer". These must not collide.

- The phi-specific turn-protocol mechanics (`PROSE_NUDGE` `constants.rs:107`, `STOP_CAUSE::Prose`, `TURN_SCARCITY_NUDGE gated on turn_cap` `builder.rs:138`) live in **code** (`run_loop`), not in the `build_system_prompt` section list — so reordering static sections *cannot* overwrite them. Good.
- Enforce in the slot order: `identity` + `mechanics` remain **authoritative on the interaction protocol** (probe → plan in prose → repl blocks → don't finalize early). `doing_tasks` / `output_efficiency` govern **content quality only** (terse, no filler), not turn protocol. Keep output-efficiency physically *after* mechanics so "lead with the answer" reads as "when you do answer, lead with it", not "answer immediately". (Decision-3 gating already keeps doing-tasks out of the bare-RLM path where this tension is sharpest.)

Acceptance: a test asserting `DEFAULT_MECHANICS`' "Do not flip … on turn 1" text appears *before* the output-efficiency section in the composed prompt.

---

## Risks (carried over, verified)

- **Over-prompting.** ~8 new sections ≈ doubles the system prompt. The cache benefit that justifies claude-code's size doesn't apply to Phi's single ephemeral block — large cold content is paid every turn. **Mitigation:** the `user_prologue` gate (Decision 3) keeps coding-agent content out of the bare-RLM path.
- **Provider specificity.** `env_info` and any future second-block split are Anthropic-specific; `openai_responses` puts the whole system prompt in `instructions` with no `cache_control`. Guard env-info caching and any cache split behind the Anthropic provider check.
- **`PROSE_NUDGE` / system-reminder overlap.** Porting `getSystemRemindersSection` defines `<system-reminder>` semantics; `PROSE_NUDGE` *is* one. If Phi later auto-adds system-reminders inside repl-block outputs, the model must distinguish an instructional-in-result reminder from a turn-protocol nudge. Keep one consistent tag convention.
- **Tool-description drift** — Track 3.
- **OAuth dual-identity** — Track 0.

---

## Build & verify

- `nix develop -c cargo build -p phi-rlm -p phi-runtime -p phi-session` / `nix develop -c cargo test` (plain cargo can't link — stale rustup ld wrapper).
- Baseline tests in `crates/phi-rlm/src/prompts/tests.rs` (15 prompt-content tests) must stay green; add the new assertions listed under each track's Acceptance.
- **Verify by re-locating symbols by name** before editing any cited line (line-anchor caveat, top of file).

## Out of scope (explicit)

Section-cache registry, `SYSTEM_PROMPT_DYNAMIC_BOUNDARY`, cacheScope tiers, output-style plugins, MCP instructions, scratchpad, ant-only features, token budget, proactive/brief loop, numeric length anchors, DiscoverSkills, the second-uncached-block optimization (Decision 5 — deferred + gated), and wiring `context_files`/`skills` discovery (Track 2 note only).
