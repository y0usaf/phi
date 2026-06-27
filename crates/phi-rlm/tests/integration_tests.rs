// Integration tests for the RLM engine.

use phi_rlm::*;
use async_trait::async_trait;
use phi_ai::Usage;
use std::sync::Arc;
use std::sync::Mutex;
use std::collections::VecDeque;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const TRANSCRIPT_COMPACTION_THRESHOLD: usize = 20;

    struct ScriptedLm {
        name: String,
        responses: Mutex<VecDeque<String>>,
        prompts: Mutex<Vec<Vec<ChatMessage>>>,
        default: String,
        usage_per_call: Usage,
    }

    impl ScriptedLm {
        fn new(name: &str, responses: &[&str]) -> Self {
            Self {
                name: name.into(),
                responses: Mutex::new(responses.iter().map(|s| (*s).to_owned()).collect()),
                prompts: Mutex::new(Vec::new()),
                default: String::new(),
                usage_per_call: Usage::default(),
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

        fn recorded_prompts(&self) -> Vec<Vec<ChatMessage>> {
            self.prompts.lock().unwrap_or_else(|e| e.into_inner()).clone()
        }
    }

    #[async_trait]
    impl LanguageModel for ScriptedLm {
        fn name(&self) -> String {
            self.name.clone()
        }

        async fn complete(&self, prompt: &[ChatMessage], _cancel: &tokio_util::sync::CancellationToken) -> Result<Completion, String> {
            self.prompts.lock().unwrap_or_else(|e| e.into_inner()).push(prompt.to_vec());
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
                usage: self.usage_per_call,
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
            self.prompts.lock().unwrap_or_else(|e| e.into_inner()).push(prompt.to_vec());
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
                usage: self.usage_per_call,
            })
        }
    }

    /// Parse a raw response string: extract ```repl fences as tool calls,
    /// strip the fences from the text, and determine the stop reason.
    fn parse_response(raw: &str) -> (String, Vec<ToolCall>, StopReason) {
        let blocks = find_code_blocks(raw);
        if blocks.is_empty() {
            return (raw.to_owned(), Vec::new(), StopReason::Stop);
        }
        // Strip fences from the text, keeping surrounding prose.
        let text = split_prose(raw).join("");
        let tool_calls = blocks
            .iter()
            .enumerate()
            .map(|(i, code)| ToolCall {
                id: format!("call_{i}"),
                name: "repl".to_owned(),
                arguments: serde_json::json!({ "code": code }),
            })
            .collect();
        (text, tool_calls, StopReason::ToolUse)
    }

    fn rlm(config: RlmConfig, root: Arc<ScriptedLm>, sub: Arc<ScriptedLm>) -> Rlm {
        Rlm::new(config, root, sub, "python3".to_owned())
    }

    fn quiet_sub() -> Arc<ScriptedLm> {
        Arc::new(ScriptedLm::new("sub-model", &[]).with_default("sub default"))
    }

    #[test]
    fn system_prompt_structure() {
        // The prompt is now minimal (ref/rlm style): identity + cyber_risk +
        // core_tools + mechanics + orchestrator; no ported coding-agent
        // sections (# Doing tasks / # Executing actions / # Using your REPL
        // helpers / # Tone and style / # Output efficiency). The gate on
        // `user_prologue` is gone.
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
        assert!(bare_prompt.contains("Assist with authorized security testing"));
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
        assert_eq!(bare_prompt, coding_prompt, "user_prologue must not change the system prompt");
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

        async fn complete(&self, _prompt: &[ChatMessage], _cancel: &tokio_util::sync::CancellationToken) -> Result<Completion, String> {
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
        let tool_results: Vec<_> = prompts[1]
            .iter()
            .filter(|m| m.role == Role::Tool)
            .collect();
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
    async fn prose_silently_banked_then_repl_answer() {
        // Silent banking: a block-free turn executes nothing, but instead of
        // injecting a prose nudge, the prose is silently banked into history
        // and the loop continues. The model proceeds to code on the next turn.
        let root = Arc::new(ScriptedLm::new(
            "root-model",
            &[
                "I can answer without using the REPL.",
                "Probing now.\n```repl\nprint(context[:4])\n```",
                "```repl\nanswer['content'] = context.split()[-1]\nanswer['ready'] = True\n```",
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
        assert_eq!(result.stop, StopCause::Answer, "{:?}", result.error);
        assert_eq!(result.response, "42");
        assert_eq!(result.iterations, 3);

        let prompts = root.recorded_prompts();
        // Turn 2's prompt must contain the banked prose-only assistant message
        // from turn 1, but NO prose-nudge user reminder (silent banking).
        let second = &prompts[1];
        assert!(
            second
                .iter()
                .any(|m| m.role == Role::Assistant && m.content.contains("without using the REPL"))
        );
        assert!(
            !second
                .iter()
                .any(|m| m.role == Role::User && m.content.contains("Your previous message contained no")),
            "silent banking must not inject a prose nudge"
        );
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
    async fn max_consecutive_prose_safety_valve_fires() {
        // After MAX_CONSECUTIVE_PROSE (4) consecutive block-free turns
        // without a <task_complete> token, the safety valve fires and the
        // prose is accepted as the final answer.
        let root = Arc::new(ScriptedLm::new(
            "root-model",
            &[
                "Thinking about it.",
                "Still thinking.",
                "Almost there.",
                "The answer is 42.",
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
        assert_eq!(result.response, "The answer is 42.");
        assert_eq!(result.iterations, 4);

        let prompts = root.recorded_prompts();
        // No prose nudge should be injected on any turn, pure silent banking
        // until the threshold fires.
        for prompt in &prompts {
            assert!(
                !prompt
                    .iter()
                    .any(|m| m.role == Role::User && m.content.contains("Your previous message contained no")),
                "no prose nudge should be injected during silent banking"
            );
        }
    }

    // (second_consecutive_prose_turn_accepted_as_answer removed: the
    //  2-strike prose-bounce is replaced by silent banking + task_complete
    //  token + MAX_CONSECUTIVE_PROSE safety valve. See
    //  explicit_task_complete_token_ends_session and
    //  max_consecutive_prose_safety_valve_fires above.)

    #[tokio::test]
    async fn prose_counter_resets_after_code_turn() {
        // prose → code → prose → repl-answer: the `consecutive_prose` counter
        // must reset to 0 when a turn contains a ```repl``` block. With silent
        // banking (no nudge), both prose turns are banked silently and the
        // model proceeds to the REPL on turn 4.
        let root = Arc::new(ScriptedLm::new(
            "root-model",
            &[
                "Let me think about this without using the REPL.",
                "Actually, let me check.\n```repl\nprint('checking')\n```",
                "So the answer is clear now, without needing more code.",
                "Here it is.\n```repl\nanswer['content'] = 'done'\nanswer['ready'] = True\n```",
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
        assert_eq!(result.response, "done", "{:?}", result.error);
        assert_eq!(result.stop, StopCause::Answer, "{:?}", result.error);
        assert_eq!(result.iterations, 4);

        let prompts = root.recorded_prompts();
        // Turn 1 (prose) is banked silently, no prose nudge.
        let second = &prompts[1];
        assert!(
            !second
                .iter()
                .any(|m| m.role == Role::User && m.content.contains("Your previous message contained no")),
            "silent banking: no prose nudge after first prose"
        );
        // Turn 3 (the second prose, after the code turn reset) is also banked
        // silently, proving the counter reset, no nudge carried over.
        let fourth = &prompts[3];
        assert!(
            !fourth
                .iter()
                .any(|m| m.role == Role::User && m.content.contains("Your previous message contained no")),
            "prose after a code turn must be silently banked (counter reset, no nudge)"
        );
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

        let prompts = root.recorded_prompts();
        assert!(prompts[0].iter().any(|m| m.content.contains("Turn 1:")));
        assert!(prompts[1].iter().any(|m| m.content.contains("Turn 2:")));
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
            max_depth: 2,
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
        assert!(prompts[0].iter().any(|m| m.content.contains("Turn 1/2:")));
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
    async fn rlm_query_spawns_child_rlm() {
        // Parent and child share the same root model: responses pop in
        // call order — parent iter 1, child iters 1-2, parent iter 2.
        let root = Arc::new(
            ScriptedLm::new(
                "root-model",
                &[
                    "Delegating.\n```repl\nr = rlm_query('child task')\nprint(r)\n```",
                    "Probing.\n```repl\nprint(context)\n```",
                    "```repl\nanswer['content'] = 'child saw: ' + context\nanswer['ready'] = True\n```",
                    "```repl\nanswer['content'] = r\nanswer['ready'] = True\n```",
                ],
            )
            .with_usage(Usage {
                input: 1,
                ..Usage::default()
            }),
        );
        let config = RlmConfig {
            max_depth: 2,
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

        // The child gets the rlm_query prompt as its own context.
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
    async fn rlm_query_preview_is_single_line() {
        let root = Arc::new(ScriptedLm::new(
            "root-model",
            &[
                "```repl\nr = rlm_query(\"\"\"first line\n\nsecond\tline\"\"\")\n```",
                "```repl\nanswer['content'] = 'child ok'\nanswer['ready'] = True\n```",
                "```repl\nanswer['content'] = r\nanswer['ready'] = True\n```",
            ],
        ));
        let config = RlmConfig {
            max_depth: 2,
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
    async fn rlm_query_model_override_roots_the_child() {
        let root = Arc::new(ScriptedLm::new(
            "root-model",
            &[
                "```repl\nr = rlm_query('task', model='fast')\n```",
                "```repl\nanswer['content'] = r\nanswer['ready'] = True\n```",
            ],
        ));
        let fast = Arc::new(ScriptedLm::new(
            "fast-model",
            &["```repl\nanswer['content'] = 'from fast'\nanswer['ready'] = True\n```"],
        ));
        let config = RlmConfig {
            max_depth: 2,
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
    async fn rlm_query_at_max_depth_is_plain_completion() {
        // Default max_depth = 1: no child run, the sub model answers.
        let root = Arc::new(ScriptedLm::new(
            "root-model",
            &[
                "```repl\nr = rlm_query('q')\n```",
                "```repl\nanswer['content'] = r\nanswer['ready'] = True\n```",
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
    async fn rlm_query_budget_exhausted_blocks_child_spawn() {
        let mut usage = Usage::default();
        usage.cost.total = 1.0;
        let root = Arc::new(
            ScriptedLm::new("root-model", &[])
                .with_default("```repl\nprint(rlm_query('q'))\n```")
                .with_usage(usage),
        );
        let config = RlmConfig {
            max_depth: 2,
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
    async fn falls_back_to_plain_lm_at_max_depth() {
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

        // The compaction archive should have captured the verbatim compacted entries.
        assert!(
            !session.compaction_archive.is_empty(),
            "compaction_archive should be populated after compaction"
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

        async fn complete(&self, _prompt: &[ChatMessage], cancel: &CancellationToken) -> Result<Completion, String> {
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
