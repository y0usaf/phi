// Integration tests for the RLM engine.

use async_trait::async_trait;
use phi_ai::Usage;
use phi_rlm::*;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const TRANSCRIPT_COMPACTION_THRESHOLD: usize = 20;

struct ScriptedLm {
    name: String,
    responses: Mutex<VecDeque<String>>,
    prompts: Mutex<Vec<Vec<ChatMessage>>>,
    default: String,
    usage_per_call: Usage,
    /// Per-call usage overrides, popped in call order; falls back to
    /// `usage_per_call` when exhausted. Lets compaction tests model the
    /// input-token drop that follows a successful compaction.
    usage_sequence: Mutex<VecDeque<Usage>>,
    context_window: Option<u64>,
}

impl ScriptedLm {
    fn new(name: &str, responses: &[&str]) -> Self {
        Self {
            name: name.into(),
            responses: Mutex::new(responses.iter().map(|s| (*s).to_owned()).collect()),
            prompts: Mutex::new(Vec::new()),
            default: String::new(),
            usage_per_call: Usage::default(),
            usage_sequence: Mutex::new(VecDeque::new()),
            context_window: None,
        }
    }

    fn with_default(mut self, default: &str) -> Self {
        self.default = default.into();
        self
    }

    fn with_usage(mut self, usage: Usage) -> Self {
        self.usage_per_call = usage;
        self
    }

    fn with_context_window(mut self, window: u64) -> Self {
        self.context_window = Some(window);
        self
    }

    fn with_usages(self, usages: &[Usage]) -> Self {
        *self.usage_sequence.lock().unwrap() = usages.iter().copied().collect();
        self
    }

    fn next_usage(&self) -> Usage {
        self.usage_sequence
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
            .unwrap_or(self.usage_per_call)
    }

    fn recorded_prompts(&self) -> Vec<Vec<ChatMessage>> {
        self.prompts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

#[async_trait]
impl LanguageModel for ScriptedLm {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn context_window(&self) -> Option<u64> {
        self.context_window
    }

    async fn complete(
        &self,
        prompt: &[ChatMessage],
        _cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Completion, String> {
        self.prompts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(prompt.to_vec());
        let raw = self
            .responses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
            .unwrap_or_else(|| self.default.clone());
        let (text, tool_calls, stop_reason) = parse_response(&raw);
        Ok(Completion {
            text,
            tool_calls,
            stop_reason,
            usage: self.next_usage(),
        })
    }

    async fn complete_streaming(
        &self,
        prompt: &[ChatMessage],
        on_delta: DeltaSink,
        _on_thinking: Option<ThinkingSink>,
        on_tool_call: Option<ToolCallSink>,
        _cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Completion, String> {
        self.prompts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(prompt.to_vec());
        let raw = self
            .responses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
            .unwrap_or_else(|| self.default.clone());
        let (text, tool_calls, stop_reason) = parse_response(&raw);

        // Emit text deltas.
        if !text.is_empty() {
            on_delta(&text);
        }

        // Emit tool call events.
        if let Some(sink) = &on_tool_call {
            for tc in &tool_calls {
                sink(ToolCallEvent::End {
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                    arguments: tc.arguments.clone(),
                });
            }
        }

        Ok(Completion {
            text,
            tool_calls,
            stop_reason,
            usage: self.next_usage(),
        })
    }
}

/// Parse a raw response string: extract ```repl fences (Python code) and
/// ```tool:NAME fences (JSON arguments) as tool calls, strip the fences from
/// the text, and determine the stop reason.
fn parse_response(raw: &str) -> (String, Vec<ToolCall>, StopReason) {
    let (text, blocks) = extract_fences(raw);
    if blocks.is_empty() {
        return (raw.to_owned(), Vec::new(), StopReason::Stop);
    }
    let tool_calls = blocks
        .iter()
        .enumerate()
        .map(|(i, (name, args))| ToolCall {
            id: format!("call_{i}"),
            name: name.clone(),
            arguments: args.clone(),
        })
        .collect();
    (text, tool_calls, StopReason::ToolUse)
}

/// Local fence parser for the test harness. Returns (prose-without-fences,
/// extracted (tool name, arguments) pairs). A simple state machine: it scans
/// line by line, toggling in/out of a fenced block; ```repl bodies become the
/// `repl` tool's `code` argument, ```tool:NAME bodies are parsed as JSON
/// arguments for tool NAME.
fn extract_fences(raw: &str) -> (String, Vec<(String, serde_json::Value)>) {
    let mut prose = String::new();
    let mut blocks = Vec::new();
    let mut in_block: Option<String> = None;
    let mut current = String::new();
    for line in raw.split_inclusive('\n') {
        let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
        if in_block.is_none() && trimmed.starts_with("```repl") {
            in_block = Some("repl".to_owned());
            current.clear();
        } else if in_block.is_none() && trimmed.starts_with("```tool:") {
            in_block = Some(trimmed.trim_start_matches("```tool:").trim().to_owned());
            current.clear();
        } else if let Some(name) = in_block.take_if(|_| trimmed.starts_with("```")) {
            let args = if name == "repl" {
                serde_json::json!({ "code": current.trim() })
            } else {
                serde_json::from_str(current.trim()).expect("valid JSON in tool fence")
            };
            blocks.push((name, args));
            current.clear();
        } else if in_block.is_some() {
            current.push_str(line);
        } else {
            prose.push_str(line);
        }
    }
    (prose, blocks)
}

fn rlm(config: RlmConfig, root: Arc<ScriptedLm>, sub: Arc<ScriptedLm>) -> Rlm {
    Rlm::new(config, root, sub, "python3".to_owned())
}

fn quiet_sub() -> Arc<ScriptedLm> {
    Arc::new(ScriptedLm::new("sub-model", &[]).with_default("sub default"))
}

#[test]
fn system_prompt_structure() {
    // The prompt is now minimal (ref/rlm style): identity + core_tools +
    // mechanics + orchestrator; no ported coding-agent sections (# Doing
    // tasks / # Executing actions / # Using your REPL helpers / # Tone and
    // style / # Output efficiency) and no cyber-risk section (refs ship
    // none). The gate on `user_prologue` is gone.
    let bare = rlm(
        RlmConfig {
            env_info: Some("# Environment\n - Primary working directory: /x".into()),
            ..RlmConfig::default()
        },
        quiet_sub(),
        quiet_sub(),
    );
    let bare_prompt = build_system_prompt(&bare.system_prompt_options());
    assert!(!bare_prompt.contains("# Doing tasks"));
    assert!(!bare_prompt.contains("# Executing actions with care"));
    assert!(!bare_prompt.contains("# Using your REPL helpers"));
    assert!(!bare_prompt.contains("# Tone and style"));
    assert!(!bare_prompt.contains("# Output efficiency"));
    assert!(!bare_prompt.contains("Assist with authorized security testing"));
    assert!(bare_prompt.contains("Available in the REPL:"));
    assert!(bare_prompt.contains("# Environment"));
    assert!(bare_prompt.contains("As an RLM, you should act as an orchestrator"));

    // `user_prologue` no longer toggles any system-prompt section; it is
    // only inserted as a user message in `initial_history`.
    let coding = rlm(
        RlmConfig {
            env_info: Some("# Environment\n - Primary working directory: /x".into()),
            user_prologue: Some("You are a coding agent.".into()),
            ..RlmConfig::default()
        },
        quiet_sub(),
        quiet_sub(),
    );
    let coding_prompt = build_system_prompt(&coding.system_prompt_options());
    assert_eq!(
        bare_prompt, coding_prompt,
        "user_prologue must not change the system prompt"
    );
}

#[tokio::test]
async fn answers_via_answer_protocol() {
    let root = Arc::new(ScriptedLm::new(
        "root-model",
        &[
            "Probing.\n```repl\nprint(context[:20])\n```",
            "Submitting.\n```repl\nanswer['content'] = context.split()[-1]\nanswer['ready'] = True\n```",
        ],
    ));
    let engine = rlm(RlmConfig::default(), root.clone(), quiet_sub());
    let result = engine
        .completion(
            ContextPayload::Str("The magic number is 42".into()),
            Some("What is the magic number?"),
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(result.stop, StopCause::Answer);
    assert_eq!(result.response, "42");
    assert_eq!(result.iterations, 2);

    let prompts = root.recorded_prompts();
    assert_eq!(prompts[0][0].role, Role::System);
    assert!(
        prompts[0][0]
            .content
            .starts_with("You are a Recursive Language Model")
    );
    assert!(
        prompts[0][1]
            .content
            .starts_with("Answer the following: What is the magic number?")
    );
    // Second turn sees the tool result of the first.
    let second = &prompts[1];
    assert!(second.iter().any(|m| m.role == Role::Tool));
}

#[tokio::test]
async fn routes_llm_query_subcalls() {
    let root = Arc::new(ScriptedLm::new(
        "root-model",
        &[
            "```repl\nr = llm_query('what?')\nprint(r)\n```",
            "```repl\nanswer['content'] = r\nanswer['ready'] = True\n```",
        ],
    ));
    let sub = Arc::new(ScriptedLm::new("sub-model", &[]).with_default("sub says hi"));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let engine = rlm(RlmConfig::default(), root.clone(), sub).with_events(tx);
    let result = engine
        .completion(
            ContextPayload::Str("ctx".into()),
            None,
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(result.stop, StopCause::Answer);
    assert_eq!(result.response, "sub says hi");

    let mut saw_subcall = false;
    let mut saw_root_text = false;
    while let Ok(event) = rx.try_recv() {
        match event {
            RlmEvent::SubcallStart { model, .. } => {
                assert_eq!(model, "sub-model");
                saw_subcall = true;
            }
            RlmEvent::RootText { .. } => saw_root_text = true,
            _ => {}
        }
    }
    assert!(saw_subcall);
    assert!(saw_root_text);
}

/// Streams a fence-closing chunk, then refuses to finish the stream until
/// Emits a tool call during the stream.
struct EagerLm {}

#[async_trait]
impl LanguageModel for EagerLm {
    fn name(&self) -> String {
        "eager-model".into()
    }

    async fn complete(
        &self,
        _prompt: &[ChatMessage],
        _cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Completion, String> {
        Err("streaming expected".into())
    }

    async fn complete_streaming(
        &self,
        _prompt: &[ChatMessage],
        on_delta: DeltaSink,
        _on_thinking: Option<ThinkingSink>,
        on_tool_call: Option<phi_rlm::ToolCallSink>,
        _cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Completion, String> {
        on_delta("Running.");
        let code = "answer['content'] = 'done'\nanswer['ready'] = True";
        let arguments = serde_json::json!({ "code": code });
        if let Some(sink) = &on_tool_call {
            sink(ToolCallEvent::End {
                id: "call_0".to_owned(),
                name: "repl".to_owned(),
                arguments: arguments.clone(),
            });
        }
        tokio::task::yield_now().await;
        Ok(Completion {
            text: "Running.".to_owned(),
            tool_calls: vec![ToolCall {
                id: "call_0".to_owned(),
                name: "repl".to_owned(),
                arguments,
            }],
            stop_reason: StopReason::ToolUse,
            usage: Usage::default(),
        })
    }
}

#[tokio::test]
async fn executes_blocks_while_streaming() {
    let exec_started = Arc::new(tokio::sync::Notify::new());
    let root = Arc::new(EagerLm {});
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let notify = exec_started.clone();
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            if matches!(event, RlmEvent::CodeExecStart { .. }) {
                notify.notify_one();
            }
        }
    });
    let engine = Rlm::new(
        RlmConfig::default(),
        root,
        quiet_sub(),
        "python3".to_owned(),
    )
    .with_events(tx);
    let result = engine
        .completion(
            ContextPayload::Str("ctx".into()),
            None,
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(result.stop, StopCause::Answer, "{:?}", result.error);
    assert_eq!(result.response, "done");
}

#[tokio::test]
async fn multi_block_outputs_batch_together() {
    let root = Arc::new(ScriptedLm::new(
        "root-model",
        &[
            "Two at once.\n```repl\nprint('alpha')\n```\n```repl\nprint('beta')\n```",
            "```repl\nanswer['content'] = 'ok'\nanswer['ready'] = True\n```",
        ],
    ));
    let engine = rlm(RlmConfig::default(), root.clone(), quiet_sub());
    let result = engine
        .completion(
            ContextPayload::Str("ctx".into()),
            None,
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(result.stop, StopCause::Answer, "{:?}", result.error);

    // Both block outputs land as tool results on the next turn.
    let prompts = root.recorded_prompts();
    let tool_results: Vec<_> = prompts[1].iter().filter(|m| m.role == Role::Tool).collect();
    assert_eq!(tool_results.len(), 2, "two tool results expected");
    assert!(tool_results[0].content.contains("alpha"));
    assert!(tool_results[1].content.contains("beta"));
}

#[tokio::test]
async fn prose_and_tool_call_in_same_turn() {
    let root = Arc::new(ScriptedLm::new(
        "root-model",
        &[
            "Plan.\n```repl\nprint('x')\n```",
            "```repl\nanswer['content'] = 'ok'\nanswer['ready'] = True\n```",
        ],
    ));
    let engine = rlm(RlmConfig::default(), root.clone(), quiet_sub());
    let result = engine
        .completion(
            ContextPayload::Str("ctx".into()),
            None,
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(result.stop, StopCause::Answer, "{:?}", result.error);

    // The assistant turn in history has the prose text and tool calls.
    let prompts = root.recorded_prompts();
    let assistant = prompts[1]
        .iter()
        .find(|m| m.role == Role::Assistant)
        .expect("assistant message");
    assert!(assistant.content.contains("Plan."), "{}", assistant.content);
    assert!(!assistant.tool_calls.is_empty(), "should have tool calls");
}

#[tokio::test]
async fn first_prose_turn_is_final_answer() {
    // WS9.1: a no-tool-call, non-empty turn IS the final answer, immediately
    // — even on turn 1. No banking, no nudge, no extra completions.
    let root = Arc::new(ScriptedLm::new(
        "root-model",
        &["The magic number is 42.", "NEVER REACHED"],
    ));
    let engine = rlm(RlmConfig::default(), root.clone(), quiet_sub());
    let result = engine
        .completion(
            ContextPayload::Str("The magic number is 42".into()),
            Some("What is the magic number?"),
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(result.stop, StopCause::Prose, "{:?}", result.error);
    assert_eq!(result.response, "The magic number is 42.");
    assert_eq!(result.iterations, 1, "zero wasted end-of-task completions");
    assert_eq!(root.recorded_prompts().len(), 1);
}

#[tokio::test]
async fn explicit_task_complete_token_ends_session() {
    // A block-free turn with `<task_complete output="..."/>` is accepted
    // as the final answer via `StopCause::Prose`. The `output` attribute
    // is extracted as the response.
    let root = Arc::new(ScriptedLm::new(
        "root-model",
        &[
            "Let me think about this.\n```repl\nprint('checking')\n```",
            "The answer is clear now. <task_complete output=\"42 is the magic number\"/>",
        ],
    ));
    let engine = rlm(RlmConfig::default(), root.clone(), quiet_sub());
    let result = engine
        .completion(
            ContextPayload::Str("ctx".into()),
            Some("What is the magic number?"),
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(result.stop, StopCause::Prose, "{:?}", result.error);
    assert_eq!(result.response, "42 is the magic number");
    assert_eq!(result.iterations, 2);
}

#[tokio::test]
async fn prose_after_code_turns_ends_run() {
    // Code turns, then a prose turn: the prose is the final answer on the
    // spot (WS9.1) — the model followed the system prompt ("stop making tool
    // calls and output your answer as text") and pays no extra completions.
    let root = Arc::new(ScriptedLm::new(
        "root-model",
        &[
            "Probing.\n```repl\nprint(context[:10])\n```",
            "The context says the magic number is 42.",
            "NEVER REACHED",
        ],
    ));
    let engine = rlm(RlmConfig::default(), root.clone(), quiet_sub());
    let result = engine
        .completion(
            ContextPayload::Str("The magic number is 42".into()),
            Some("What is the magic number?"),
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(result.stop, StopCause::Prose, "{:?}", result.error);
    assert_eq!(result.response, "The context says the magic number is 42.");
    assert_eq!(result.iterations, 2);
}

#[tokio::test]
async fn empty_turn_nudged_then_recovers() {
    // An empty turn (no tool calls, no text) cannot be an answer: an
    // EMPTY_TURN_NUDGE user message is injected and the loop continues,
    // giving the model a chance to recover.
    let root = Arc::new(ScriptedLm::new(
        "root-model",
        &[
            "",
            "```repl\nanswer['content'] = 'recovered'\nanswer['ready'] = True\n```",
        ],
    ));
    let engine = rlm(RlmConfig::default(), root.clone(), quiet_sub());
    let result = engine
        .completion(
            ContextPayload::Str("ctx".into()),
            Some("What is the answer?"),
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(result.stop, StopCause::Answer, "{:?}", result.error);
    assert_eq!(result.response, "recovered");
    assert_eq!(result.iterations, 2);

    let prompts = root.recorded_prompts();
    // The nudge lands in turn 2's prompt; nothing is banked for the empty
    // turn itself (an empty assistant message is provider-hostile).
    let second = &prompts[1];
    assert!(
        second
            .iter()
            .any(|m| m.role == Role::User && m.content.contains("Your previous turn was empty")),
        "empty-turn nudge expected in turn 2's prompt"
    );
    assert!(
        !second
            .iter()
            .any(|m| m.role == Role::Assistant && m.content.is_empty()),
        "no empty assistant message banked"
    );
}

#[tokio::test]
async fn consecutive_empty_turns_fail_run() {
    // MAX_CONSECUTIVE_EMPTY (3) empty turns in a row fail the run — an empty
    // string can never be accepted as an answer, so failing beats spinning.
    let root = Arc::new(ScriptedLm::new("root-model", &[]).with_default(""));
    let engine = rlm(RlmConfig::default(), root.clone(), quiet_sub());
    let result = engine
        .completion(
            ContextPayload::Str("ctx".into()),
            Some("What is the answer?"),
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(result.stop, StopCause::Failed);
    assert!(
        result
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("consecutive empty model turns"),
        "{:?}",
        result.error
    );
    assert_eq!(result.iterations, 3);
}

#[tokio::test]
async fn root_ignores_iteration_limit() {
    let root = Arc::new(ScriptedLm::new(
        "root-model",
        &[
            "turn 1\n```repl\nprint('one')\n```",
            "turn 2\n```repl\nprint('two')\n```",
            "turn 3\n```repl\nanswer['content'] = 'done'\nanswer['ready'] = True\n```",
        ],
    ));
    let config = RlmConfig {
        max_iterations: 1,
        ..RlmConfig::default()
    };
    let engine = rlm(config, root.clone(), quiet_sub());
    let result = engine
        .completion(
            ContextPayload::Str("ctx".into()),
            None,
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(result.stop, StopCause::Answer, "{:?}", result.error);
    assert_eq!(result.response, "done");
    assert_eq!(result.iterations, 3);

    // WS9.2: the uncapped root gets no per-turn scaffolding at all — the
    // only user message in any turn's prompt is the initial one, and later
    // turns continue straight from the tool results.
    let prompts = root.recorded_prompts();
    for prompt in &prompts {
        assert_eq!(
            prompt.iter().filter(|m| m.role == Role::User).count(),
            1,
            "no per-turn user messages expected"
        );
    }
    assert_eq!(prompts[1].last().unwrap().role, Role::Tool);
}

#[tokio::test]
async fn recursive_child_default_answer_on_iteration_exhaustion() {
    // The child runs repl blocks each turn (so the prose-bounce does not
    // fire) and exhausts its turn cap; the parent then synthesizes a
    // final answer via the `DEFAULT_ANSWER_NUDGE` injection.
    let root = Arc::new(ScriptedLm::new(
        "root-model",
        &[
            "thinking 1\n```repl\nprint('hi')\n```",
            "thinking 2\n```repl\nprint('hi')\n```",
            "the synthesized final",
        ],
    ));
    let config = RlmConfig {
        max_rlm_depth: 2,
        max_iterations: 2,
        ..RlmConfig::default()
    };
    let engine = rlm(config, root.clone(), quiet_sub()).at_depth(1);
    let result = engine
        .completion(
            ContextPayload::Str("ctx".into()),
            None,
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(result.stop, StopCause::DefaultAnswer);
    assert_eq!(result.response, "the synthesized final");

    let prompts = root.recorded_prompts();
    // The cap lives in the system prompt (WS9.2), and with 2 turns remaining
    // out of 2 (≤ TURN_WARNING_THRESHOLD) the remaining-turns reminder is
    // included from turn 1.
    assert!(
        prompts[0]
            .iter()
            .any(|m| m.role == Role::System && m.content.contains("hard cap of 2 turns"))
    );
    assert!(
        prompts[0]
            .iter()
            .any(|m| m.role == Role::User && m.content.contains("turn 1/2 — 2 turn(s) remaining"))
    );
    let last = prompts.last().unwrap().last().unwrap();
    assert_eq!(last.role, Role::Assistant);
    assert_eq!(last.content, DEFAULT_ANSWER_NUDGE);
}

#[tokio::test]
async fn consecutive_errors_hit_limit() {
    let root = Arc::new(
        ScriptedLm::new("root-model", &[]).with_default("trying\n```repl\nundefined_fn()\n```"),
    );
    let config = RlmConfig {
        max_errors: Some(2),
        max_iterations: 5,
        ..RlmConfig::default()
    };
    let engine = rlm(config, root, quiet_sub());
    let result = engine
        .completion(
            ContextPayload::Str("ctx".into()),
            None,
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(result.stop, StopCause::Limit(LimitKind::Errors));
    assert_eq!(result.iterations, 2);
    assert!(result.response.starts_with("trying"));
    assert!(result.error.unwrap().contains("Error threshold exceeded"));
}

#[tokio::test]
async fn budget_limit_stops_run() {
    let mut usage = Usage::default();
    usage.cost.total = 1.0;
    let root = Arc::new(
        ScriptedLm::new("root-model", &[])
            .with_default("partial progress")
            .with_usage(usage),
    );
    let config = RlmConfig {
        max_budget: Some(0.5),
        ..RlmConfig::default()
    };
    let engine = rlm(config, root, quiet_sub());
    let result = engine
        .completion(
            ContextPayload::Str("ctx".into()),
            None,
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(result.stop, StopCause::Limit(LimitKind::Budget));
    assert_eq!(result.response, "partial progress");
    assert!(result.usage.cost.total >= 1.0);
}

#[tokio::test]
async fn persistent_session_returns_per_turn_usage_deltas() {
    let root = Arc::new(
        ScriptedLm::new(
            "root-model",
            &[
                "```repl\nanswer['content'] = 'first'\nanswer['ready'] = True\n```",
                "```repl\nanswer['content'] = 'second'\nanswer['ready'] = True\n```",
            ],
        )
        .with_usage(Usage {
            input: 10,
            output: 2,
            total_tokens: 12,
            ..Usage::default()
        }),
    );
    let engine = rlm(RlmConfig::default(), root, quiet_sub());
    let mut session = RlmSession::new(engine, ContextPayload::Str("ctx".into()));

    let first = session.send("one", &CancellationToken::new()).await;
    let second = session.send("two", &CancellationToken::new()).await;

    assert_eq!(first.stop, StopCause::Answer, "{:?}", first.error);
    assert_eq!(second.stop, StopCause::Answer, "{:?}", second.error);
    assert_eq!(first.usage.input, 10);
    assert_eq!(second.usage.input, 10);
    assert_eq!(session.usage().input, 20);
    assert_eq!(second.usage_by_model.len(), 1);
    assert_eq!(second.usage_by_model[0].model, "root-model");
    assert_eq!(second.usage_by_model[0].usage.input, 10);
}

#[tokio::test]
async fn timeout_checked_before_iteration() {
    let root = Arc::new(ScriptedLm::new("root-model", &[]).with_default("never used"));
    let config = RlmConfig {
        max_timeout: Some(Duration::ZERO),
        ..RlmConfig::default()
    };
    let engine = rlm(config, root.clone(), quiet_sub());
    let result = engine
        .completion(
            ContextPayload::Str("ctx".into()),
            None,
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(result.stop, StopCause::Limit(LimitKind::Timeout));
    assert_eq!(result.iterations, 0);
    assert!(root.recorded_prompts().is_empty());
}

#[tokio::test]
async fn subagent_spawns_child_rlm() {
    // Parent and child share the same root model: responses pop in
    // call order — parent iter 1, child iters 1-2, parent iter 2.
    // subagent() now returns a dict; REPL code accesses r['text'].
    let root = Arc::new(
        ScriptedLm::new(
            "root-model",
            &[
                "Delegating.\n```repl\nr = subagent('child task')\nprint(r['text'])\n```",
                "Probing.\n```repl\nprint(context)\n```",
                "```repl\nanswer['content'] = 'child saw: ' + context\nanswer['ready'] = True\n```",
                "```repl\nanswer['content'] = r['text']\nanswer['ready'] = True\n```",
            ],
        )
        .with_usage(Usage {
            input: 1,
            ..Usage::default()
        }),
    );
    let config = RlmConfig {
        max_rlm_depth: 2,
        ..RlmConfig::default()
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let engine = rlm(config, root.clone(), quiet_sub()).with_events(tx);
    let result = engine
        .completion(
            ContextPayload::Str("parent ctx".into()),
            None,
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(result.stop, StopCause::Answer, "{:?}", result.error);
    assert_eq!(result.response, "child saw: child task");
    // Child usage (2 calls) rolls up into the parent's (2 calls), and
    // remains attributed to the actual model instead of a single label.
    assert_eq!(result.usage.input, 4);
    assert_eq!(result.usage_by_model.len(), 1);
    assert_eq!(result.usage_by_model[0].model, "root-model");
    assert_eq!(result.usage_by_model[0].usage.input, 4);

    // The child gets the subagent prompt as its own context.
    let prompts = root.recorded_prompts();
    assert!(
        prompts[1][1]
            .content
            .contains("Your context is a str of 10 total characters"),
        "{}",
        prompts[1][1].content
    );

    let mut saw_child_run = false;
    let mut saw_child_answer = false;
    let mut subcall_model = None;
    while let Ok(event) = rx.try_recv() {
        match event {
            RlmEvent::RunStart { depth: 1 } => saw_child_run = true,
            RlmEvent::RunEnd { depth: 1, stop, .. } => assert_eq!(stop, StopCause::Answer),
            RlmEvent::AnswerSubmitted { depth: 1, .. } => saw_child_answer = true,
            RlmEvent::SubcallStart {
                depth: 1, model, ..
            } => subcall_model = Some(model),
            _ => {}
        }
    }
    assert!(saw_child_run);
    assert!(saw_child_answer);
    assert_eq!(subcall_model.as_deref(), Some("root-model"));
}

#[tokio::test]
async fn llm_query_preview_is_single_line() {
    let root = Arc::new(ScriptedLm::new(
        "root-model",
        &[
            "```repl\nr = llm_query(\"\"\"first line\n\nsecond\tline\"\"\")\n```",
            "```repl\nanswer['content'] = r\nanswer['ready'] = True\n```",
        ],
    ));
    let sub = Arc::new(ScriptedLm::new("sub-model", &[]).with_default("ok"));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let engine = rlm(RlmConfig::default(), root, sub).with_events(tx);
    let result = engine
        .completion(
            ContextPayload::Str("ctx".into()),
            None,
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(result.stop, StopCause::Answer, "{:?}", result.error);

    let mut preview = None;
    while let Ok(event) = rx.try_recv() {
        if let RlmEvent::SubcallStart {
            depth: 1,
            prompt_preview,
            ..
        } = event
        {
            preview = Some(prompt_preview);
        }
    }
    assert_eq!(preview.as_deref(), Some("first line second line"));
}

#[tokio::test]
async fn subagent_preview_is_single_line() {
    // subagent() returns a dict; the test only checks the preview event,
    // so we access r['text'] to form the parent's final answer.
    let root = Arc::new(ScriptedLm::new(
        "root-model",
        &[
            "```repl\nr = subagent(\"\"\"first line\n\nsecond\tline\"\"\")\n```",
            "```repl\nanswer['content'] = 'child ok'\nanswer['ready'] = True\n```",
            "```repl\nanswer['content'] = r['text']\nanswer['ready'] = True\n```",
        ],
    ));
    let config = RlmConfig {
        max_rlm_depth: 2,
        ..RlmConfig::default()
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let engine = rlm(config, root, quiet_sub()).with_events(tx);
    let result = engine
        .completion(
            ContextPayload::Str("ctx".into()),
            None,
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(result.stop, StopCause::Answer, "{:?}", result.error);

    let mut preview = None;
    while let Ok(event) = rx.try_recv() {
        if let RlmEvent::SubcallStart {
            depth: 1,
            prompt_preview,
            ..
        } = event
        {
            preview = Some(prompt_preview);
        }
    }
    assert_eq!(preview.as_deref(), Some("first line second line"));
}

#[tokio::test]
async fn subagent_model_override_roots_the_child() {
    // subagent() returns a dict; extract text for the final answer.
    let root = Arc::new(ScriptedLm::new(
        "root-model",
        &[
            "```repl\nr = subagent('task', model='fast')\n```",
            "```repl\nanswer['content'] = r['text']\nanswer['ready'] = True\n```",
        ],
    ));
    let fast = Arc::new(ScriptedLm::new(
        "fast-model",
        &["```repl\nanswer['content'] = 'from fast'\nanswer['ready'] = True\n```"],
    ));
    let config = RlmConfig {
        max_rlm_depth: 2,
        ..RlmConfig::default()
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut engine = rlm(config, root, quiet_sub());
    engine.register_model("fast", fast);
    let engine = engine.with_events(tx);
    let result = engine
        .completion(
            ContextPayload::Str("ctx".into()),
            None,
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(result.stop, StopCause::Answer, "{:?}", result.error);
    assert_eq!(result.response, "from fast");

    let mut subcall_model = None;
    while let Ok(event) = rx.try_recv() {
        if let RlmEvent::SubcallStart {
            depth: 1, model, ..
        } = event
        {
            subcall_model = Some(model);
        }
    }
    assert_eq!(subcall_model.as_deref(), Some("fast-model"));
}

#[tokio::test]
async fn subagent_at_max_rlm_depth_is_plain_completion() {
    // Default max_rlm_depth = 1: no child run, the sub model answers.
    // subagent() returns a dict with stop='fallback'; extract r['text'].
    let root = Arc::new(ScriptedLm::new(
        "root-model",
        &[
            "```repl\nr = subagent('q')\n```",
            "```repl\nanswer['content'] = r['text']\nanswer['ready'] = True\n```",
        ],
    ));
    let sub = Arc::new(ScriptedLm::new("sub-model", &[]).with_default("sub says deep"));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let engine = rlm(RlmConfig::default(), root, sub).with_events(tx);
    let result = engine
        .completion(
            ContextPayload::Str("ctx".into()),
            None,
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(result.stop, StopCause::Answer, "{:?}", result.error);
    assert_eq!(result.response, "sub says deep");

    while let Ok(event) = rx.try_recv() {
        assert!(
            !matches!(event, RlmEvent::RunStart { depth: 1 }),
            "no child RLM at max depth"
        );
    }
}

#[tokio::test]
async fn subagent_budget_exhausted_blocks_child_spawn() {
    let mut usage = Usage::default();
    usage.cost.total = 1.0;
    let root = Arc::new(
        ScriptedLm::new("root-model", &[])
            .with_default("```repl\nprint(subagent('q'))\n```")
            .with_usage(usage),
    );
    let config = RlmConfig {
        max_rlm_depth: 2,
        max_budget: Some(0.5),
        ..RlmConfig::default()
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let engine = rlm(config, root, quiet_sub()).with_events(tx);
    let result = engine
        .completion(
            ContextPayload::Str("ctx".into()),
            None,
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(result.stop, StopCause::Limit(LimitKind::Budget));

    let mut saw_exhausted = false;
    while let Ok(event) = rx.try_recv() {
        match event {
            RlmEvent::CodeExecEnd { stdout, .. } => {
                saw_exhausted |= stdout.contains("Error: Budget exhausted");
            }
            RlmEvent::RunStart { depth } => assert_eq!(depth, 0),
            _ => {}
        }
    }
    assert!(saw_exhausted, "REPL should see the budget-exhausted error");
}

#[tokio::test]
async fn falls_back_to_plain_lm_at_max_rlm_depth() {
    let root = Arc::new(ScriptedLm::new("root-model", &["plain answer"]));
    let engine = rlm(RlmConfig::default(), root.clone(), quiet_sub()).at_depth(1);
    let result = engine
        .completion(
            ContextPayload::Str("just answer this".into()),
            None,
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(result.stop, StopCause::Fallback);
    assert_eq!(result.response, "plain answer");
    let prompts = root.recorded_prompts();
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0][0].content, "just answer this");
}

#[tokio::test]
async fn session_persists_env_and_exposes_history() {
    let root = Arc::new(ScriptedLm::new(
        "root-model",
        &[
            // Turn 1: stash a variable, answer.
            "```repl\nstash = 'kept'\nanswer['content'] = 'first answer'\nanswer['ready'] = True\n```",
            // Turn 2: prove the env persisted and history is visible.
            "```repl\nprint(stash)\nprint(history[-1]['content'])\n```",
            "```repl\nanswer['content'] = 'second answer'\nanswer['ready'] = True\n```",
        ],
    ));
    let engine = rlm(RlmConfig::default(), root.clone(), quiet_sub());
    let mut session = RlmSession::new(engine, ContextPayload::Str("ctx".into()));
    let cancel = CancellationToken::new();

    let first = session.send("question one", &cancel).await;
    assert_eq!(first.stop, StopCause::Answer);
    assert_eq!(first.response, "first answer");

    let second = session.send("question two", &cancel).await;
    assert_eq!(second.stop, StopCause::Answer);
    assert_eq!(second.response, "second answer");

    let prompts = root.recorded_prompts();
    // Turn 2's first user prompt mentions the prior history variable.
    let turn2_user = &prompts[1]
        .iter()
        .map(|m| m.content.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        turn2_user.contains("1 prior conversation history"),
        "{turn2_user}"
    );
    // Turn 2's second root call sees the REPL output proving persistence.
    let outputs = prompts[2]
        .iter()
        .map(|m| m.content.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(outputs.contains("kept"), "{outputs}");
    assert!(outputs.contains("first answer"), "{outputs}");

    session.shutdown().await;
}

#[tokio::test]
async fn session_exec_shell_records_repl_turn() {
    let root = Arc::new(ScriptedLm::new(
        "root-model",
        &["```repl\nanswer['content'] = 'done'\nanswer['ready'] = True\n```"],
    ));
    let engine = rlm(RlmConfig::default(), root.clone(), quiet_sub());
    let mut session = RlmSession::new(engine, ContextPayload::Str("ctx".into()));
    let cancel = CancellationToken::new();

    let recorded = session
        .exec_shell("print('shell out')", true)
        .await
        .unwrap();
    assert!(recorded.stdout.contains("shell out"));
    let silent = session.exec_shell("print('quiet')", false).await.unwrap();
    assert!(silent.stdout.contains("quiet"));

    let result = session.send("next task", &cancel).await;
    assert_eq!(result.stop, StopCause::Answer);

    // The recorded execution appears as a faked repl turn; the
    // unrecorded one does not.
    let prompts = root.recorded_prompts();
    let roles: Vec<String> = prompts[0]
        .iter()
        .map(|m| format!("{:?}: {}", m.role, m.content))
        .collect();
    let joined = roles.join("\n---\n");
    assert!(
        joined.contains("Assistant: ```repl\nprint('shell out')\n```"),
        "{joined}"
    );
    assert!(joined.contains("REPL output:"), "{joined}");
    assert!(joined.contains("shell out"), "{joined}");
    assert!(!joined.contains("quiet"), "{joined}");

    session.shutdown().await;
}

#[tokio::test]
async fn session_add_context_versions_variables() {
    let root = Arc::new(ScriptedLm::new(
        "root-model",
        &["```repl\nanswer['content'] = context_1.upper()\nanswer['ready'] = True\n```"],
    ));
    let engine = rlm(RlmConfig::default(), root.clone(), quiet_sub());
    let mut session = RlmSession::new(engine, ContextPayload::Str("ctx".into()));
    let cancel = CancellationToken::new();

    let index = session.add_context("extra payload").await.unwrap();
    assert_eq!(index, 1);

    let result = session.send("use the extra context", &cancel).await;
    assert_eq!(result.stop, StopCause::Answer);
    assert_eq!(result.response, "EXTRA PAYLOAD");

    // The per-turn prompt advertises all three contexts: context_0
    // (payload), context_1 (manual add_context), context_2 (auto-loaded
    // prompt from send).
    let prompts = root.recorded_prompts();
    let joined = prompts[0]
        .iter()
        .map(|m| m.content.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("3 contexts available"), "{joined}");

    session.shutdown().await;
}

#[tokio::test]
async fn transcript_compaction_folds_old_exchanges_into_summary() {
    let root = Arc::new(ScriptedLm::new("root-model", &[]).with_default(
        "```repl
answer['content'] = 'ok'
answer['ready'] = True
```",
    ));
    let engine = rlm(RlmConfig::default(), root, quiet_sub());
    let mut session = RlmSession::new(engine, ContextPayload::Str("ctx".into()));
    let cancel = CancellationToken::new();

    // Send enough exchanges to exceed the compaction threshold (20 pairs).
    for i in 0..25 {
        let _ = session.send(&format!("question {i}"), &cancel).await;
    }

    // After compaction, the in-band transcript should be shorter than
    // the full 25 pairs, and session_summary should be populated.
    let pairs = session.transcript.len() / 2;
    assert!(
        pairs <= TRANSCRIPT_COMPACTION_THRESHOLD,
        "transcript has {pairs} pairs, expected <= {TRANSCRIPT_COMPACTION_THRESHOLD}"
    );
    assert!(
        session.session_summary.is_some(),
        "session_summary should be populated after compaction"
    );
    let summary = session.session_summary.as_ref().unwrap();
    assert!(
        summary.contains("question 0"),
        "summary should contain the oldest compacted exchange"
    );
    assert!(
        !summary.contains("question 24"),
        "summary should not contain the most recent exchange (still in transcript)"
    );

    session.shutdown().await;
}

/// A model that blocks forever during streaming until the cancel token
/// fires. If the cancel token never reaches `complete_streaming`, the
/// test hangs and times out — proving the wiring is correct.
struct BlockingLm;

#[async_trait]
impl LanguageModel for BlockingLm {
    fn name(&self) -> String {
        "blocking-model".into()
    }

    async fn complete(
        &self,
        _prompt: &[ChatMessage],
        cancel: &CancellationToken,
    ) -> Result<Completion, String> {
        cancel.cancelled().await;
        Err("aborted".into())
    }

    async fn complete_streaming(
        &self,
        _prompt: &[ChatMessage],
        _on_delta: DeltaSink,
        _on_thinking: Option<ThinkingSink>,
        _on_tool_call: Option<phi_rlm::ToolCallSink>,
        cancel: &CancellationToken,
    ) -> Result<Completion, String> {
        cancel.cancelled().await;
        Err("aborted".into())
    }
}

#[tokio::test]
async fn cancel_returns_promptly_during_streaming() {
    let root: Arc<dyn LanguageModel> = Arc::new(BlockingLm);
    let sub: Arc<dyn LanguageModel> = Arc::new(BlockingLm);
    let engine = Rlm::new(RlmConfig::default(), root, sub, "python3".to_owned());
    let cancel = CancellationToken::new();

    // Fire cancel after a short delay to simulate the user pressing Esc.
    let fire = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        fire.cancel();
    });

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        engine.completion(ContextPayload::Str("ctx".into()), None, &cancel),
    )
    .await;

    let result = result.expect("completion should return within 5s, not hang");
    assert_eq!(
        result.stop,
        StopCause::Cancelled,
        "expected Cancelled, got {:?}: {:?}",
        result.stop,
        result.error,
    );
}

#[tokio::test]
async fn subagent_returns_structured_dict_with_stop_and_ok() {
    // Verify the dict contract: subagent() returns {'text', 'stop', 'iterations', 'ok'}.
    // A child that answers normally has stop='answer' and ok=True.
    // Responses are popped in call order: parent iter 1 fires the subagent,
    // the child answers in 1 iteration, then parent iter 2 inspects the dict.
    let root = Arc::new(ScriptedLm::new(
        "root-model",
        &[
            // Parent iter 1: delegate.
            "```repl\nr = subagent('what is 2+2?')\n```",
            // Child iter 1 (runs inside subagent above).
            "```repl\nanswer['content'] = 'four'\nanswer['ready'] = True\n```",
            // Parent iter 2: inspect the dict fields.
            "```repl\nanswer['content'] = '%s|%s|%s|%s' % (r['text'], r['stop'], r['iterations'], r['ok'])\nanswer['ready'] = True\n```",
        ],
    ));
    let config = RlmConfig {
        max_rlm_depth: 2,
        ..RlmConfig::default()
    };
    let engine = rlm(config, root.clone(), quiet_sub());
    let result = engine
        .completion(
            ContextPayload::Str("ctx".into()),
            None,
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(result.stop, StopCause::Answer, "{:?}", result.error);
    // The answer encodes the dict fields: text|stop|iterations|ok.
    let parts: Vec<&str> = result.response.split('|').collect();
    assert_eq!(
        parts.len(),
        4,
        "expected 4 pipe-separated fields: {}",
        result.response
    );
    assert_eq!(parts[0], "four", "text should be the child's answer");
    assert_eq!(
        parts[1], "answer",
        "stop should be 'answer' for a clean finish"
    );
    assert_eq!(parts[2], "1", "child answered in 1 iteration");
    assert_eq!(parts[3], "True", "ok should be True for a clean answer");
}

#[tokio::test]
async fn subagent_summary_is_child_authored_when_set() {
    // WS7.3: a child that sets answer['summary'] delivers it to the parent
    // as r['summary'], verbatim (it's under the 4K bound). The summary is a
    // standing declaration — here it rides out on the answer path.
    let root = Arc::new(ScriptedLm::new(
        "root-model",
        &[
            // Parent iter 1: delegate.
            "```repl\nr = subagent('do a thing')\n```",
            // Child iter 1: report + answer.
            "```repl\nanswer['summary'] = 'checked 3 files; no issues'\nanswer['content'] = 'long detailed answer'\nanswer['ready'] = True\n```",
            // Parent iter 2: inspect summary and text.
            "```repl\nanswer['content'] = r['summary'] + '|' + r['text']\nanswer['ready'] = True\n```",
        ],
    ));
    let config = RlmConfig {
        max_rlm_depth: 2,
        ..RlmConfig::default()
    };
    let engine = rlm(config, root, quiet_sub());
    let result = engine
        .completion(
            ContextPayload::Str("ctx".into()),
            None,
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(result.stop, StopCause::Answer, "{:?}", result.error);
    assert_eq!(
        result.response,
        "checked 3 files; no issues|long detailed answer"
    );
}

#[tokio::test]
async fn subagent_summary_survives_prose_finish_and_derives_when_unset() {
    // WS7.3, both halves of the contract in one run:
    // (a) a summary set before a *prose* finish still reaches the parent —
    //     it is not tied to answer['ready'];
    // (b) a child that never sets one gets a derived summary (bounded head
    //     of text — equal to text when text is short).
    let root = Arc::new(ScriptedLm::new(
        "root-model",
        &[
            // Parent iter 1: delegate twice.
            "```repl\na = subagent('task one')\nb = subagent('task two')\n```",
            // Child A iter 1: declare the summary, keep working.
            "```repl\nanswer['summary'] = 'report from child A'\nprint('working')\n```",
            // Child A iter 2: finish with prose (WS9.1 path).
            "full prose answer from child A",
            // Child B iter 1: prose finish, no summary ever set.
            "short answer from B",
            // Parent iter 2: inspect both.
            "```repl\nanswer['content'] = a['summary'] + '|' + b['summary'] + '|' + b['text']\nanswer['ready'] = True\n```",
        ],
    ));
    let config = RlmConfig {
        max_rlm_depth: 2,
        ..RlmConfig::default()
    };
    let engine = rlm(config, root, quiet_sub());
    let result = engine
        .completion(
            ContextPayload::Str("ctx".into()),
            None,
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(result.stop, StopCause::Answer, "{:?}", result.error);
    assert_eq!(
        result.response,
        "report from child A|short answer from B|short answer from B"
    );
}

#[tokio::test]
async fn subagent_summary_is_bounded_at_4k() {
    // WS7.3: an oversized child-authored summary is truncated engine-side
    // with an explicit marker — the bound is enforced by the host, never
    // trusted to the model (jcode's normalize_completion_report).
    let root = Arc::new(ScriptedLm::new(
        "root-model",
        &[
            "```repl\nr = subagent('big report')\n```",
            // Child: a 6,000-char summary.
            "```repl\nanswer['summary'] = 'x' * 6000\nanswer['content'] = 'done'\nanswer['ready'] = True\n```",
            "```repl\nanswer['content'] = '%d|%s' % (len(r['summary']), 'truncated' in r['summary'])\nanswer['ready'] = True\n```",
        ],
    ));
    let config = RlmConfig {
        max_rlm_depth: 2,
        ..RlmConfig::default()
    };
    let engine = rlm(config, root, quiet_sub());
    let result = engine
        .completion(
            ContextPayload::Str("ctx".into()),
            None,
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(result.stop, StopCause::Answer, "{:?}", result.error);
    assert_eq!(
        result.response, "4000|True",
        "summary must be exactly the 4K bound with the truncation marker"
    );
}

#[tokio::test]
async fn subagent_depth_cap_returns_fallback_stop() {
    // At max_rlm_depth the engine degrades to a flat one-shot; the dict
    // must carry stop='fallback' and ok=True (error-free flat completion).
    let root = Arc::new(ScriptedLm::new(
        "root-model",
        &[
            "```repl\nr = subagent('q')\n```",
            "```repl\nanswer['content'] = r['stop'] + '|' + str(r['ok']) + '|' + str(r['iterations'])\nanswer['ready'] = True\n```",
        ],
    ));
    let sub = Arc::new(ScriptedLm::new("sub-model", &[]).with_default("flat answer"));
    let engine = rlm(RlmConfig::default(), root, sub);
    // depth=0, max_rlm_depth=1 → next_depth=1 >= 1, triggers the fallback.
    let result = engine
        .completion(
            ContextPayload::Str("ctx".into()),
            None,
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(result.stop, StopCause::Answer, "{:?}", result.error);
    // stop='fallback', ok=True (no error), iterations=0 (flat call, no loop).
    assert_eq!(result.response, "fallback|True|0");
}

#[tokio::test]
async fn subagent_ok_false_on_turn_exhaustion() {
    // A child that exhausts its turns returns stop='default_answer' and ok=False.
    // This validates that the parent can detect incomplete work programmatically.
    // Responses are popped in call order: parent iter 1 fires the subagent,
    // the child runs to exhaustion (2 repl turns + nudge response), then
    // the parent's iter 2 inspects the dict.
    let root = Arc::new(ScriptedLm::new(
        "root-model",
        &[
            // Parent iter 1: delegate.
            "```repl\nr = subagent('hard task')\n```",
            // Child iter 1 (child runs inside subagent above).
            "thinking 1\n```repl\nprint('hi')\n```",
            // Child iter 2.
            "thinking 2\n```repl\nprint('hi')\n```",
            // Child: synthesized answer after DEFAULT_ANSWER_NUDGE (no repl block).
            "best guess after exhaustion",
            // Parent iter 2: inspect the dict.
            "```repl\nanswer['content'] = str(r['ok']) + '|' + r['stop']\nanswer['ready'] = True\n```",
        ],
    ));
    let config = RlmConfig {
        max_rlm_depth: 2,
        max_iterations: 2,
        ..RlmConfig::default()
    };
    let engine = rlm(config, root.clone(), quiet_sub());
    let result = engine
        .completion(
            ContextPayload::Str("ctx".into()),
            None,
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(result.stop, StopCause::Answer, "{:?}", result.error);
    // ok=False because the child stopped with DefaultAnswer (turn-starved).
    assert_eq!(result.response, "False|default_answer");
}

#[tokio::test]
async fn restore_session_recreates_context_variables() {
    // After restore_session, context_N variables must exist for each
    // restored exchange's prompt and the contexts counter must be advanced
    // so the next live send doesn't overwrite an existing context slot.
    // The model's response reads context_1 to verify it was restored.
    let root = Arc::new(ScriptedLm::new(
        "root-model",
        &["```repl
answer['content'] = context_1
answer['ready'] = True
```"],
    ));
    let engine = rlm(RlmConfig::default(), root.clone(), quiet_sub());
    let mut session = RlmSession::new(engine, ContextPayload::Str("payload".into()));
    let cancel = CancellationToken::new();

    // Simulate a restored session with one completed exchange.
    let exchanges = vec![RestoredExchange {
        pre_shell: vec![],
        prompt: "what is the answer".to_owned(),
        repls: vec![RestoredRepl {
            prose: String::new(),
            code: "x = 1".to_owned(),
            stdout: String::new(),
            stderr: String::new(),
            subcalls: vec![],
        }],
        response: "the answer is 42".to_owned(),
        compactions: vec![],
    }];
    session.restore_session(&exchanges, &[]).await.unwrap();

    // After restore, contexts should be 2 (context_0 = payload, context_1 = restored prompt)
    assert_eq!(
        session.contexts, 2,
        "contexts counter must advance past restored exchanges"
    );

    // The next live send should see context_1 (restored prompt) and
    // create context_2 (the new prompt), not overwrite context_1.
    let result = session.send("echo the context", &cancel).await;
    assert_eq!(result.stop, StopCause::Answer);
    assert_eq!(result.response, "what is the answer");

    // Verify the prompt advertised 3 contexts (0=payload, 1=restored, 2=new)
    let prompts = root.recorded_prompts();
    let joined = prompts[0]
        .iter()
        .map(|m| m.content.clone())
        .collect::<Vec<_>>()
        .join(
            "
",
        );
    assert!(
        joined.contains("3 contexts available"),
        "should advertise 3 contexts after restore + new send, got: {joined}"
    );

    session.shutdown().await;
}

#[tokio::test]
async fn restore_session_preserves_interrupted_exchange_variables() {
    // An interrupted exchange (no assistant response) must still be
    // restored so its REPL variables and history_N are available.
    let root = Arc::new(ScriptedLm::new(
        "root-model",
        &["```repl
answer['content'] = str(history_0[0]['content'])
answer['ready'] = True
```"],
    ));
    let engine = rlm(RlmConfig::default(), root, quiet_sub());
    let mut session = RlmSession::new(engine, ContextPayload::Str("payload".into()));
    let cancel = CancellationToken::new();

    // One interrupted exchange: prompt + repl, but empty response.
    let exchanges = vec![RestoredExchange {
        pre_shell: vec![],
        prompt: "do some work".to_owned(),
        repls: vec![RestoredRepl {
            prose: String::new(),
            code: "result = 'work done'".to_owned(),
            stdout: String::new(),
            stderr: String::new(),
            subcalls: vec![],
        }],
        response: String::new(),
        compactions: vec![],
    }];
    session.restore_session(&exchanges, &[]).await.unwrap();

    // The exchange counter must have advanced despite no response.
    assert_eq!(
        session.exchanges, 1,
        "interrupted exchange must still be restored as history_0"
    );

    // history_0 should exist and contain the user prompt as the first message.
    let result = session.send("check history", &cancel).await;
    assert_eq!(result.stop, StopCause::Answer);
    assert!(
        result.response.contains("do some work"),
        "history_0 should contain the interrupted exchange's prompt, got: {}",
        result.response
    );

    session.shutdown().await;
}

// ── In-loop context compaction (WS5) ────────────────────────────────────────

/// Usage large enough to cross the compaction threshold for a 100K window:
/// limit = 0.85 · 100_000 − 20_000 headroom = 65_000.
fn overflow_usage() -> Usage {
    Usage {
        input: 90_000,
        ..Usage::default()
    }
}

/// Usage safely below the compaction threshold.
fn small_usage() -> Usage {
    Usage {
        input: 10_000,
        ..Usage::default()
    }
}

#[tokio::test]
async fn compaction_folds_history_and_archives_span() {
    // Every root call reports 90K input tokens against a 100K window, so the
    // threshold is crossed from iteration 0. Iteration 0 has no compactable
    // span yet (only the preserved head + the current iteration); after
    // iteration 1 the engine must compact: consume one scripted response as
    // the summary, rebuild history around it, and archive the compacted span
    // into the REPL as `compacted_0_0`.
    let root = Arc::new(
        ScriptedLm::new(
            "root-model",
            &[
                "Step one.\n```repl\nx = 41\n```",
                "Step two.\n```repl\ny = x + 1\n```",
                "COMPACTION SUMMARY: computed x=41, y=42; next, submit y.",
                "Checking the archive.\n```repl\nprint(compacted_0_0[0]['role'], len(compacted_0_0))\n```",
                "Submitting.\n```repl\nanswer['content'] = str(y)\nanswer['ready'] = True\n```",
            ],
        )
        // Call order: turn 0 (bail: no compactable span yet), turn 1
        // (triggers compaction), the summary call, then post-compaction
        // turns whose input has dropped back below the threshold.
        .with_usages(&[
            overflow_usage(),
            overflow_usage(),
            small_usage(),
            small_usage(),
            small_usage(),
        ])
        .with_context_window(100_000),
    );
    let engine = rlm(RlmConfig::default(), root.clone(), quiet_sub());
    let result = engine
        .completion(
            ContextPayload::Str("ctx".into()),
            Some("Compute the number."),
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(result.stop, StopCause::Answer, "{:?}", result.error);
    assert_eq!(result.response, "42", "REPL vars must survive compaction");

    let prompts = root.recorded_prompts();
    // Calls: turn 0, turn 1, summary, turn 2, turn 3 (answer).
    assert_eq!(prompts.len(), 5);

    // The summary request carries the structured-section prompt.
    let summary_request = &prompts[2];
    assert!(
        summary_request
            .last()
            .unwrap()
            .content
            .contains("Task & intent"),
        "summary call must use the structured compaction prompt"
    );

    // The first post-compaction turn sees the rebuilt history: summary +
    // continue message present, the compacted span's content gone.
    let post = &prompts[3];
    assert!(
        post.iter()
            .any(|m| m.role == Role::Assistant && m.content.contains("COMPACTION SUMMARY")),
        "rebuilt history must carry the summary as an assistant message"
    );
    assert!(
        post.iter().any(|m| m.role == Role::User
            && m.content.contains("[Context compacted")
            && m.content.contains("compacted_0_0")),
        "rebuilt history must carry the continue message naming the archive variable"
    );
    assert!(
        !post.iter().any(|m| m.content.contains("Step one.")),
        "the compacted span must be gone from the in-band history"
    );
    // The preserved head (system + initial prompt) survives verbatim.
    assert_eq!(post[0].role, Role::System);
    assert!(post[1].content.contains("Compute the number."));

    // The archive variable is a real, populated REPL list: printing its
    // first role produced output visible in the next turn's tool result.
    // With no per-turn user messages (WS9.2), the compacted span starts at
    // the turn-0 assistant message.
    let last = prompts.last().unwrap();
    assert!(
        last.iter()
            .any(|m| m.role == Role::Tool && m.content.contains("assistant")),
        "compacted_0_0 must exist in the REPL and hold message dicts"
    );
}

#[tokio::test]
async fn compaction_breaker_disarms_after_repeated_failures() {
    // The summary call yields prose-free empty text (a bare tool call), which
    // counts as a compaction failure. After MAX_COMPACTION_FAILURES (3)
    // failures the breaker disarms: no further summary calls are attempted
    // and the run proceeds to completion untouched.
    let root = Arc::new(
        ScriptedLm::new(
            "root-model",
            &[
                "Step one.\n```repl\na = 1\n```",
                "Step two.\n```repl\nb = 2\n```",
                // Summary attempt 1: empty text (a tool call, no prose).
                "```repl\nprint('not a summary')\n```",
                "Step three.\n```repl\nc = 3\n```",
                // Summary attempt 2.
                "```repl\nprint('still not a summary')\n```",
                "Step four.\n```repl\nd = 4\n```",
                // Summary attempt 3 — breaker trips after this one.
                "```repl\nprint('nope')\n```",
                "Step five.\n```repl\ne = 5\n```",
                "Submitting.\n```repl\nanswer['content'] = 'done'\nanswer['ready'] = True\n```",
            ],
        )
        // Failed compactions never shrink the history, so every loop turn
        // keeps reporting overflow; only the summary calls themselves are
        // small. Call order: t0, t1, s1, t2, s2, t3, s3, t4, t5.
        .with_usages(&[
            overflow_usage(),
            overflow_usage(),
            small_usage(),
            overflow_usage(),
            small_usage(),
            overflow_usage(),
            small_usage(),
            overflow_usage(),
            overflow_usage(),
        ])
        .with_context_window(100_000),
    );
    let engine = rlm(RlmConfig::default(), root.clone(), quiet_sub());
    let result = engine
        .completion(
            ContextPayload::Str("ctx".into()),
            Some("Count."),
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(result.stop, StopCause::Answer, "{:?}", result.error);

    let prompts = root.recorded_prompts();
    // 6 loop turns + exactly 3 summary attempts: had the breaker not
    // tripped, turns after the third failure would keep consuming summary
    // responses and the script would desynchronize.
    assert_eq!(prompts.len(), 9);
    // History was never rebuilt: the final turn still contains turn 0.
    assert!(
        prompts
            .last()
            .unwrap()
            .iter()
            .any(|m| m.content.contains("Step one.")),
        "failed compaction must leave history untouched"
    );
}

#[tokio::test]
async fn compaction_disabled_without_context_window() {
    // No context_window on the model → the threshold check is off: the
    // scripted flow runs with no summary call consumed even at huge usage.
    let root = Arc::new(
        ScriptedLm::new(
            "root-model",
            &[
                "Step one.\n```repl\nx = 1\n```",
                "Step two.\n```repl\nx += 1\n```",
                "Submitting.\n```repl\nanswer['content'] = str(x)\nanswer['ready'] = True\n```",
            ],
        )
        .with_usage(overflow_usage()),
    );
    let engine = rlm(RlmConfig::default(), root.clone(), quiet_sub());
    let result = engine
        .completion(
            ContextPayload::Str("ctx".into()),
            Some("Count."),
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(result.stop, StopCause::Answer, "{:?}", result.error);
    assert_eq!(result.response, "2");
    assert_eq!(root.recorded_prompts().len(), 3, "no summary call expected");
}

// ── WS9.3a: tool surface through the extension API ──────────────────────────

/// A test extension registering:
/// - `greet`: native-exposed, handler-dispatched (the MCP/WS7.1 shape)
/// - `shout`: native-exposed, Python-dispatched (the coding-helper shape;
///   the function itself is defined by `setup_code` in the test config)
struct NativeToolsExt;

impl phi_ext::Extension for NativeToolsExt {
    fn manifest(&self) -> phi_ext::ExtensionManifest {
        phi_ext::ExtensionManifest {
            id: "test.native-tools".to_owned(),
            name: "NativeTools".to_owned(),
            version: "0".to_owned(),
            description: "WS9.3a test tools".to_owned(),
        }
    }

    fn register(&self, host: &mut dyn phi_ext::ExtensionHost) -> anyhow::Result<()> {
        host.register_tool(phi_ext::ToolSpec {
            name: "greet".to_owned(),
            description: "Greets someone".to_owned(),
            params: vec![phi_ext::ToolParam {
                name: "who".to_owned(),
                ty: "str".to_owned(),
                default: None,
            }],
            expose: phi_ext::ToolExposure::Native,
            input_schema: None,
            dispatch: phi_ext::ToolDispatch::Handler(Arc::new(|inv| {
                let args: serde_json::Value = serde_json::from_str(&inv.input)?;
                Ok(phi_ext::ToolOutput {
                    content: format!("Hello, {}!", args["who"].as_str().unwrap_or("?")),
                })
            })),
        })?;
        host.register_tool(phi_ext::ToolSpec {
            name: "shout".to_owned(),
            description: "Uppercases text".to_owned(),
            params: vec![phi_ext::ToolParam {
                name: "text".to_owned(),
                ty: "str".to_owned(),
                default: None,
            }],
            expose: phi_ext::ToolExposure::Both,
            input_schema: None,
            dispatch: phi_ext::ToolDispatch::Python,
        })?;
        host.register_tool(phi_ext::ToolSpec::repl(
            "echo_tool",
            "Echoes its input",
            vec![phi_ext::ToolParam {
                name: "text".to_owned(),
                ty: "str".to_owned(),
                default: None,
            }],
            Arc::new(|inv| {
                Ok(phi_ext::ToolOutput {
                    content: format!("echo:{}", inv.input),
                })
            }),
        ))?;
        Ok(())
    }
}

fn native_tools_runtime() -> Arc<phi_ext::AppRuntime> {
    Arc::new(
        phi_ext::RuntimeBuilder::new()
            .with_cwd(std::path::Path::new("."))
            .register_extension(NativeToolsExt)
            .build()
            .expect("build runtime"),
    )
}

/// Find the tool_result message contents in a recorded prompt.
fn tool_results(prompt: &[ChatMessage]) -> Vec<String> {
    prompt
        .iter()
        .filter(|m| m.role == Role::Tool)
        .map(|m| m.content.clone())
        .collect()
}

#[tokio::test]
async fn native_handler_tool_round_trips_through_loop() {
    // Turn 1 calls the handler-dispatched `greet` tool natively; the handler
    // output must come back as that call's tool_result in turn 2's prompt.
    let root = Arc::new(ScriptedLm::new(
        "root-model",
        &[
            "Greeting.\n```tool:greet\n{\"who\": \"world\"}\n```",
            "The tool said hello.",
        ],
    ));
    let engine =
        rlm(RlmConfig::default(), root.clone(), quiet_sub()).with_runtime(native_tools_runtime());
    let result = engine
        .completion(
            ContextPayload::Str("ctx".into()),
            Some("Greet the world."),
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(result.stop, StopCause::Prose, "{:?}", result.error);
    assert_eq!(result.response, "The tool said hello.");

    let prompts = root.recorded_prompts();
    assert_eq!(prompts.len(), 2);
    let results = tool_results(&prompts[1]);
    assert_eq!(results.len(), 1);
    assert!(
        results[0].contains("Hello, world!"),
        "tool_result missing handler output: {results:?}"
    );
}

#[tokio::test]
async fn native_python_tool_executes_in_the_repl_env() {
    // `shout` is Python-dispatched: the native call maps onto the same-named
    // function defined in the env (here via setup_code, standing in for the
    // prelude). One implementation, two exposures.
    let root = Arc::new(ScriptedLm::new(
        "root-model",
        &[
            "Shouting.\n```tool:shout\n{\"text\": \"hello \\\"quoted\\\" world\"}\n```",
            "Shouted.",
        ],
    ));
    let config = RlmConfig {
        setup_code: Some("def shout(text):\n    return str(text).upper()".to_owned()),
        ..RlmConfig::default()
    };
    let engine = rlm(config, root.clone(), quiet_sub()).with_runtime(native_tools_runtime());
    let result = engine
        .completion(
            ContextPayload::Str("ctx".into()),
            Some("Shout it."),
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(result.stop, StopCause::Prose, "{:?}", result.error);

    let prompts = root.recorded_prompts();
    assert_eq!(prompts.len(), 2);
    let results = tool_results(&prompts[1]);
    assert_eq!(results.len(), 1);
    assert!(
        results[0].contains("HELLO \"QUOTED\" WORLD"),
        "python-dispatched result missing (JSON-literal escaping broken?): {results:?}"
    );
}

#[tokio::test]
async fn unknown_native_tool_gets_synthetic_error_result() {
    // A tool call whose name isn't `repl` and isn't a native-exposed registry
    // tool is not executed, but the provider must still see a tool_result.
    let root = Arc::new(ScriptedLm::new(
        "root-model",
        &["Trying.\n```tool:nonexistent\n{}\n```", "Gave up."],
    ));
    let engine =
        rlm(RlmConfig::default(), root.clone(), quiet_sub()).with_runtime(native_tools_runtime());
    let result = engine
        .completion(
            ContextPayload::Str("ctx".into()),
            Some("Try a bogus tool."),
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(result.stop, StopCause::Prose, "{:?}", result.error);

    let prompts = root.recorded_prompts();
    assert_eq!(prompts.len(), 2);
    let results = tool_results(&prompts[1]);
    assert_eq!(results.len(), 1);
    assert!(
        results[0].contains("was not executed"),
        "missing synthetic error result: {results:?}"
    );
}

#[tokio::test]
async fn repl_stub_reaches_extension_handler() {
    // The other exposure: a repl-only extension tool called from Python via
    // its generated stub, which round-trips through `__phi_ext_tool__` to the
    // host handler. (Regression guard: the stub target used to be undefined
    // in the env, so every stub call died with a NameError.)
    let runtime = native_tools_runtime();
    let config = RlmConfig {
        setup_code: Some(runtime.extension_prelude_stubs()),
        ..RlmConfig::default()
    };
    let root = Arc::new(ScriptedLm::new(
        "root-model",
        &[
            "Echoing.\n```repl\nprint(echo_tool(text=\"hi\"))\n```",
            "Echoed.",
        ],
    ));
    let engine = rlm(config, root.clone(), quiet_sub()).with_runtime(runtime);
    let result = engine
        .completion(
            ContextPayload::Str("ctx".into()),
            Some("Echo hi."),
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(result.stop, StopCause::Prose, "{:?}", result.error);

    let prompts = root.recorded_prompts();
    assert_eq!(prompts.len(), 2);
    let results = tool_results(&prompts[1]);
    assert_eq!(results.len(), 1);
    assert!(
        results[0].contains("echo:") && results[0].contains("\"text\""),
        "stub call did not reach the handler: {results:?}"
    );
    assert!(
        !results[0].contains("NameError"),
        "__phi_ext_tool__ undefined in env: {results:?}"
    );
}

#[tokio::test]
async fn register_tools_includes_native_registry_tools() {
    // The per-turn tool list is registry-built: repl always (bare core must
    // boot), plus every native-exposed tool. Repl-only tools stay off it.
    struct CapturingLm(Mutex<Vec<phi_ai::ToolDefinition>>);
    #[async_trait]
    impl LanguageModel for CapturingLm {
        fn name(&self) -> String {
            "capturing".to_owned()
        }
        fn set_tools(&self, tools: Vec<phi_ai::ToolDefinition>) {
            *self.0.lock().unwrap() = tools;
        }
        async fn complete(
            &self,
            _prompt: &[ChatMessage],
            _cancel: &tokio_util::sync::CancellationToken,
        ) -> Result<Completion, String> {
            Err("unused".into())
        }
    }

    let root = Arc::new(CapturingLm(Mutex::new(Vec::new())));
    let engine = Rlm::new(
        RlmConfig::default(),
        root.clone(),
        quiet_sub(),
        "python3".to_owned(),
    )
    .with_runtime(native_tools_runtime());
    engine.register_tools();

    let tools = root.0.lock().unwrap().clone();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, vec!["repl", "greet", "shout"]);
    let greet = &tools[1];
    assert_eq!(greet.input_schema["properties"]["who"]["type"], "string");
    assert_eq!(greet.input_schema["required"][0], "who");
}

// ── WS1.2: agent env knobs (repl_tool / context) ─────────────────────────────

/// A test extension that registers an agent profile with a given env block.
struct AgentExt {
    name: &'static str,
    env: Option<phi_ext::AgentEnv>,
}

impl phi_ext::Extension for AgentExt {
    fn manifest(&self) -> phi_ext::ExtensionManifest {
        phi_ext::ExtensionManifest {
            id: format!("test.agent-{}", self.name),
            name: format!("Agent{}", self.name),
            version: "0".to_owned(),
            description: "WS1.2 test agent".to_owned(),
        }
    }
    fn register(&self, host: &mut dyn phi_ext::ExtensionHost) -> anyhow::Result<()> {
        let mut spec = phi_ext::AgentSpec::new(self.name);
        if let Some(env) = &self.env {
            spec = spec.with_env(env.clone());
        }
        host.register_agent(spec)
    }
}

/// Build a runtime with the native tools plus an agent profile, then activate
/// the agent. The native tools let us observe whether `repl` is still in the
/// per-turn tool list alongside them.
fn agent_runtime(name: &'static str, env: Option<phi_ext::AgentEnv>) -> Arc<phi_ext::AppRuntime> {
    let rt = Arc::new(
        phi_ext::RuntimeBuilder::new()
            .with_cwd(std::path::Path::new("."))
            .register_extension(NativeToolsExt)
            .register_extension(AgentExt { name, env })
            .build()
            .expect("build runtime"),
    );
    rt.set_active_agent(Some(name)).expect("activate agent");
    rt
}

/// `env.repl_tool = false` removes the engine-owned `repl` tool from the
/// model's per-turn surface; the native registry tools remain. This is the
/// pi-shaped profile's flat tool-call loop (no REPL surface).
#[tokio::test]
async fn register_tools_omits_repl_when_agent_env_repl_tool_false() {
    struct CapturingLm(Mutex<Vec<phi_ai::ToolDefinition>>);
    #[async_trait]
    impl LanguageModel for CapturingLm {
        fn name(&self) -> String {
            "capturing".to_owned()
        }
        fn set_tools(&self, tools: Vec<phi_ai::ToolDefinition>) {
            *self.0.lock().unwrap() = tools;
        }
        async fn complete(
            &self,
            _prompt: &[ChatMessage],
            _cancel: &tokio_util::sync::CancellationToken,
        ) -> Result<Completion, String> {
            Err("unused".into())
        }
    }

    let root = Arc::new(CapturingLm(Mutex::new(Vec::new())));
    let engine = Rlm::new(
        RlmConfig::default(),
        root.clone(),
        quiet_sub(),
        "python3".to_owned(),
    )
    .with_runtime(agent_runtime(
        "pi",
        Some(phi_ext::AgentEnv {
            repl_tool: Some(false),
            context: Some(false),
        }),
    ));
    engine.register_tools();

    let tools = root.0.lock().unwrap().clone();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    // `repl` is gone; the native registry tools remain.
    assert!(
        !names.contains(&"repl"),
        "repl tool should be hidden: {names:?}"
    );
    assert_eq!(names, vec!["greet", "shout"]);
}

/// The default (no agent active, or an agent with an empty env) keeps the
/// `repl` tool — today's behavior, so bare core boots. This is the fall-
/// through guarantee: `None`/absent knobs inherit the engine default.
#[tokio::test]
async fn register_tools_keeps_repl_for_default_agent() {
    struct CapturingLm(Mutex<Vec<phi_ai::ToolDefinition>>);
    #[async_trait]
    impl LanguageModel for CapturingLm {
        fn name(&self) -> String {
            "capturing".to_owned()
        }
        fn set_tools(&self, tools: Vec<phi_ai::ToolDefinition>) {
            *self.0.lock().unwrap() = tools;
        }
        async fn complete(
            &self,
            _prompt: &[ChatMessage],
            _cancel: &tokio_util::sync::CancellationToken,
        ) -> Result<Completion, String> {
            Err("unused".into())
        }
    }

    let root = Arc::new(CapturingLm(Mutex::new(Vec::new())));
    // An agent with an empty env block — both knobs absent → inherit (on).
    let engine = Rlm::new(
        RlmConfig::default(),
        root.clone(),
        quiet_sub(),
        "python3".to_owned(),
    )
    .with_runtime(agent_runtime("rlm", None));
    engine.register_tools();

    let tools = root.0.lock().unwrap().clone();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, vec!["repl", "greet", "shout"]);
}

/// `env.context = false` skips `context` / `context_0` injection into the
/// REPL env: a `repl` call that references `context_0` raises a NameError
/// instead of printing the payload. The payload still reaches the user
/// prompt (this gate is only about the REPL-side variable).
#[tokio::test]
async fn env_context_false_skips_context_injection() {
    let root = Arc::new(ScriptedLm::new(
        "root-model",
        &[
            // Probe whether context_0 is defined in the REPL namespace.
            "Probe.\n```repl\ntry:\n    context_0\n    print('HAS_CONTEXT')\nexcept NameError:\n    print('NO_CONTEXT')\n```",
            // Second turn: accept the probe result as the answer.
            "done",
        ],
    ));
    let engine = rlm(RlmConfig::default(), root.clone(), quiet_sub()).with_runtime(agent_runtime(
        "pi",
        Some(phi_ext::AgentEnv {
            repl_tool: None,
            context: Some(false),
        }),
    ));
    let result = engine
        .completion(
            ContextPayload::Str("the context payload".into()),
            Some("Probe the env."),
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(result.stop, StopCause::Prose, "{:?}", result.error);
    // The repl tool result for turn 1 must report NO_CONTEXT (context_0 absent).
    let results = tool_results(&root.recorded_prompts()[1]);
    assert!(
        results.iter().any(|r| r.contains("NO_CONTEXT")),
        "context_0 should be absent from the REPL env, got: {results:?}"
    );
}

/// Sanity: with the default agent (context inherited/on), `context_0` IS
/// injected — the same probe prints HAS_CONTEXT. This guards against the
/// gate accidentally inverting.
#[tokio::test]
async fn default_agent_injects_context() {
    let root = Arc::new(ScriptedLm::new(
        "root-model",
        &[
            "Probe.\n```repl\ntry:\n    context_0\n    print('HAS_CONTEXT')\nexcept NameError:\n    print('NO_CONTEXT')\n```",
            "done",
        ],
    ));
    let engine = rlm(RlmConfig::default(), root.clone(), quiet_sub())
        .with_runtime(agent_runtime("rlm", None));
    let result = engine
        .completion(
            ContextPayload::Str("the context payload".into()),
            Some("Probe the env."),
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(result.stop, StopCause::Prose, "{:?}", result.error);
    let results = tool_results(&root.recorded_prompts()[1]);
    assert!(
        results.iter().any(|r| r.contains("HAS_CONTEXT")),
        "context_0 should be present in the REPL env, got: {results:?}"
    );
}
