//! Coding-agent prelude: Python helpers injected into the REPL via
//! `RlmConfig::setup_code`, plus the system-prompt docs for them.

pub const CODING_PRELUDE: &str = include_str!("prelude.py");

pub fn coding_tools_section() -> String {
    "\
Pre-defined helper functions (already available in the REPL — call them directly, do not redefine):
- bash(cmd: str, timeout: int = 120) -> str — run a shell command; returns combined stdout/stderr plus a note on non-zero exit
- read_file(path: str, offset: int = 0, limit: int = 2000) -> str — read a file, optionally a slice of lines
- write_file(path: str, content: str) -> str — write a file, creating parent directories
- edit_file(path: str, old: str, new: str, replace_all: bool = False) -> str — exact string replacement; errors if old is missing or ambiguous
- grep(pattern: str, path: str = \".\", glob: str | None = None, max_results: int = 200) -> str — regex search across files (skips .git, node_modules, target, etc.)
- find(pattern: str, path: str = \".\", max_results: int = 200) -> str — find files by glob on filename
- ls(path: str = \".\") -> str — list a directory (dirs end with /)
- read_hashline(path: str, start_line: int = 1, limit: int = 2000) -> str — read a file as `[path#TAG]` plus `LINE:TEXT` rows for hashline-anchored edits
- grep_hashline(pattern: str, path: str = \".\", glob: str | None = None, max_results: int = 200) -> str — regex search that returns hashline headers plus numbered matching lines
- apply_hashline(patch: str) -> str — apply hashline patch sections like `[path#TAG]`, `SWAP`, `DEL`, `INS.*`, with snapshot-tag checks and stale-tag recovery
- fusion(prompt: str, analysis_models: list[str] | None = None, judge_model: str | None = None, perspectives: list[str] | None = None, recursive: bool = False, max_workers: int = 8) -> dict — run a Fusion-style multi-model/multi-perspective synthesis pass and return panel outputs plus a synthesized final answer; when specifying models, use names like `provider/model-id`

Most helpers return strings (print them to inspect); `fusion(...)` returns a dict. Output is truncated to 2000 lines / 50KB.
These run on the user's real machine in the current working directory — edits and commands have real effects.
For long-running work (builds, test suites, servers) prefer a background job, e.g. `jid = bg(\"out = bash('cargo build', timeout=3600)\")`, then poll `job_status(jid)` / `job_output(jid)` on later turns instead of blocking this one."
        .to_owned()
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use async_trait::async_trait;
    use phi_repl::{
        EnvSetup, PythonRepl, ReplEnv, SubcallHandler, SubcallRequest, SubcallResponse, ToolEvent,
    };

    use super::CODING_PRELUDE;

    #[derive(Default)]
    struct NoSubcalls {
        tool_events: Mutex<Vec<ToolEvent>>,
    }

    #[async_trait]
    impl SubcallHandler for NoSubcalls {
        async fn handle(&self, _request: SubcallRequest) -> Result<SubcallResponse, String> {
            Err("no subcalls in prelude tests".to_owned())
        }

        fn notify_tool(&self, event: ToolEvent) {
            self.tool_events.lock().unwrap().push(event);
        }
    }

    const EXEC_TIMEOUT: Duration = Duration::from_secs(20);

    async fn started() -> (PythonRepl, Arc<NoSubcalls>) {
        let handler = Arc::new(NoSubcalls::default());
        started_with(handler, EnvSetup::default()).await
    }

    async fn started_with<H>(handler: Arc<H>, mut setup: EnvSetup) -> (PythonRepl, Arc<H>)
    where
        H: SubcallHandler + 'static,
    {
        let mut repl = PythonRepl::new(handler.clone());
        setup.setup_code = Some(CODING_PRELUDE.to_owned());
        repl.start(setup).await.expect("start");
        (repl, handler)
    }

    #[derive(Default)]
    struct FusionMock {
        seen: Mutex<Vec<(String, String, Option<String>)>>,
    }

    #[async_trait]
    impl SubcallHandler for FusionMock {
        async fn handle(&self, request: SubcallRequest) -> Result<SubcallResponse, String> {
            match request {
                SubcallRequest::Llm { prompt, model } => {
                    self.seen.lock().unwrap().push((
                        "llm".to_owned(),
                        prompt.clone(),
                        model.clone(),
                    ));
                    match model.as_deref() {
                        Some("anthropic/model-a") => {
                            Ok(SubcallResponse::Text("panel A analysis".to_owned()))
                        }
                        Some("openai/model-b") => {
                            Ok(SubcallResponse::Text("panel B analysis".to_owned()))
                        }
                        Some("judge/model-c") => {
                            assert!(prompt.contains("panel A analysis"), "{prompt}");
                            assert!(prompt.contains("panel B analysis"), "{prompt}");
                            Ok(SubcallResponse::Text(
                                r#"{"consensus":["shared conclusion"],"contradictions":["none material"],"partial_coverage":["more benchmarks needed"],"unique_insights":["panel A found provenance","panel B found caveats"],"blind_spots":["runtime cost"],"final_answer":"merged final answer"}"#.to_owned(),
                            ))
                        }
                        other => Err(format!("unexpected llm model: {other:?}")),
                    }
                }
                SubcallRequest::Rlm { prompt, model } => {
                    self.seen.lock().unwrap().push((
                        "rlm".to_owned(),
                        prompt.clone(),
                        model.clone(),
                    ));
                    match model.as_deref() {
                        Some("panel/model") => {
                            Ok(SubcallResponse::Text("recursive panel analysis".to_owned()))
                        }
                        Some("judge/model") => Ok(SubcallResponse::Text(
                            r#"{"consensus":["recursive consensus"],"contradictions":[],"partial_coverage":[],"unique_insights":[],"blind_spots":[],"final_answer":"recursive final answer"}"#.to_owned(),
                        )),
                        other => Err(format!("unexpected rlm model: {other:?}; prompt={prompt}")),
                    }
                }
                SubcallRequest::HostFn { .. } => Err("unexpected host fn".to_owned()),
                SubcallRequest::ExtTool { name, .. } => Err(format!("unexpected tool: {name}")),
            }
        }
    }

    #[tokio::test]
    async fn file_helpers_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("note.txt");
        let path = path.to_str().unwrap();
        let (mut repl, _events) = started().await;

        let r = repl
            .exec(
                &format!("print(write_file({path:?}, 'hello world\\nline two'))"),
                EXEC_TIMEOUT,
            )
            .await
            .unwrap();
        assert!(r.stdout.contains("Wrote"), "stdout: {}", r.stdout);

        let r = repl
            .exec(
                &format!("print(edit_file({path:?}, 'world', 'there'))"),
                EXEC_TIMEOUT,
            )
            .await
            .unwrap();
        assert!(r.stdout.contains("Replaced 1"), "stdout: {}", r.stdout);
        assert!(r.stdout.contains("--- "), "stdout: {}", r.stdout);
        assert!(r.stdout.contains("+++ "), "stdout: {}", r.stdout);
        assert!(r.stdout.contains("-hello world"), "stdout: {}", r.stdout);
        assert!(r.stdout.contains("+hello there"), "stdout: {}", r.stdout);

        let r = repl
            .exec(&format!("print(read_file({path:?}))"), EXEC_TIMEOUT)
            .await
            .unwrap();
        assert_eq!(r.stdout, "hello there\nline two\n");

        let r = repl
            .exec(
                &format!("print(edit_file({path:?}, 'absent', 'x'))"),
                EXEC_TIMEOUT,
            )
            .await
            .unwrap();
        assert!(r.stdout.starts_with("Error:"), "stdout: {}", r.stdout);
        repl.shutdown().await;
    }

    #[tokio::test]
    async fn write_file_overwrite_returns_diff() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note.txt");
        let path = path.to_str().unwrap();
        std::fs::write(path, "alpha\nbeta\n").unwrap();
        let (mut repl, _events) = started().await;

        let r = repl
            .exec(
                &format!("print(write_file({path:?}, 'alpha\\ngamma\\n'))"),
                EXEC_TIMEOUT,
            )
            .await
            .unwrap();
        assert!(r.stdout.contains("Wrote"), "stdout: {}", r.stdout);
        assert!(r.stdout.contains("--- "), "stdout: {}", r.stdout);
        assert!(r.stdout.contains("+++ "), "stdout: {}", r.stdout);
        assert!(r.stdout.contains("-beta"), "stdout: {}", r.stdout);
        assert!(r.stdout.contains("+gamma"), "stdout: {}", r.stdout);
        repl.shutdown().await;
    }

    #[tokio::test]
    async fn bash_runs_commands() {
        let (mut repl, _events) = started().await;
        let r = repl
            .exec("print(bash('echo hi; exit 3'))", EXEC_TIMEOUT)
            .await
            .unwrap();
        assert!(r.stdout.contains("hi"), "stdout: {}", r.stdout);
        assert!(r.stdout.contains("[exit code 3]"), "stdout: {}", r.stdout);
        repl.shutdown().await;
    }

    #[tokio::test]
    async fn search_helpers_work() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "needle here\n").unwrap();
        std::fs::write(dir.path().join("b.log"), "nothing\n").unwrap();
        let root = dir.path().to_str().unwrap();
        let (mut repl, _events) = started().await;

        let r = repl
            .exec(
                &format!("print(grep('needle', path={root:?}))"),
                EXEC_TIMEOUT,
            )
            .await
            .unwrap();
        assert!(r.stdout.contains("a.txt:1"), "stdout: {}", r.stdout);

        let r = repl
            .exec(
                &format!("print(find('*.log', path={root:?}))"),
                EXEC_TIMEOUT,
            )
            .await
            .unwrap();
        assert!(r.stdout.contains("b.log"), "stdout: {}", r.stdout);

        let r = repl
            .exec(&format!("print(ls({root:?}))"), EXEC_TIMEOUT)
            .await
            .unwrap();
        assert_eq!(r.stdout, "a.txt\nb.log\n");
        repl.shutdown().await;
    }

    #[tokio::test]
    async fn hashline_helpers_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("demo.py");
        let path = path.to_str().unwrap();
        std::fs::write(path, "def greet(name):\n    return 'hi ' + name\n").unwrap();
        let (mut repl, _events) = started().await;

        let r = repl
            .exec(&format!("print(read_hashline({path:?}))"), EXEC_TIMEOUT)
            .await
            .unwrap();
        assert!(
            r.stdout.contains(&format!("[{path}#")),
            "stdout: {}",
            r.stdout
        );
        assert!(
            r.stdout.contains("1:def greet(name):"),
            "stdout: {}",
            r.stdout
        );
        assert!(
            r.stdout.contains("2:    return 'hi ' + name"),
            "stdout: {}",
            r.stdout
        );

        let patch = format!(
            r#"view = read_hashline({path:?})
header = view.splitlines()[0]
patch = header + "\nSWAP 2.=2:\n+    return 'hello ' + name\n"
print(apply_hashline(patch))"#
        );
        let r = repl.exec(&patch, EXEC_TIMEOUT).await.unwrap();
        assert!(
            r.stdout.contains(&format!("[{path}#")),
            "stdout: {}",
            r.stdout
        );
        assert!(
            r.stdout.contains("2:    return 'hello ' + name"),
            "stdout: {}",
            r.stdout
        );

        repl.shutdown().await;
    }

    #[tokio::test]
    async fn hashline_seen_line_guard_and_stale_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let seen_path = dir.path().join("seen.py");
        let seen_path = seen_path.to_str().unwrap();
        std::fs::write(seen_path, "l1\nl2\nl3\nl4\n").unwrap();

        let stale_path = dir.path().join("stale.py");
        let stale_path = stale_path.to_str().unwrap();
        std::fs::write(stale_path, "a\nb\nc\n").unwrap();

        let (mut repl, _events) = started().await;

        let r = repl
            .exec(
                &format!(
                    r#"view = read_hashline({seen_path:?}, start_line=1, limit=2)
header = view.splitlines()[0]
patch = header + "\nSWAP 4.=4:\n+L4\n"
print(apply_hashline(patch))"#
                ),
                EXEC_TIMEOUT,
            )
            .await
            .unwrap();
        assert!(r.stdout.contains("unseen line(s)"), "stdout: {}", r.stdout);

        let r = repl
            .exec(
                &format!(
                    r#"view = read_hashline({stale_path:?})
header = view.splitlines()[0]
print(apply_hashline(header + "\nSWAP 2.=2:\n+B\n"))
print(apply_hashline(header + "\nSWAP 3.=3:\n+C\n"))"#
                ),
                EXEC_TIMEOUT,
            )
            .await
            .unwrap();
        assert!(r.stdout.contains("2:B"), "stdout: {}", r.stdout);
        assert!(r.stdout.contains("3:C"), "stdout: {}", r.stdout);
        assert!(
            r.stdout.contains("Recovered from stale hash tag"),
            "stdout: {}",
            r.stdout
        );

        repl.shutdown().await;
    }

    #[tokio::test]
    async fn cwd_setting_controls_working_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("marker.txt"), "here\n").unwrap();
        let mut repl = PythonRepl::new(Arc::new(NoSubcalls::default()));
        repl.start(EnvSetup {
            setup_code: Some(CODING_PRELUDE.to_owned()),
            cwd: Some(dir.path().to_str().unwrap().to_owned()),
            ..EnvSetup::default()
        })
        .await
        .expect("start");
        let r = repl.exec("print(ls())", EXEC_TIMEOUT).await.unwrap();
        assert_eq!(r.stdout, "marker.txt\n");
        let r = repl
            .exec("print(read_file('marker.txt'))", EXEC_TIMEOUT)
            .await
            .unwrap();
        assert_eq!(r.stdout, "here\n\n");
        repl.shutdown().await;
    }

    #[tokio::test]
    async fn tools_emit_start_and_end_events() {
        let (mut repl, handler) = started().await;
        repl.exec("print(bash('echo hi'))", EXEC_TIMEOUT)
            .await
            .unwrap();
        repl.exec("print(edit_file('/nonexistent', 'a', 'b'))", EXEC_TIMEOUT)
            .await
            .unwrap();
        {
            let events = handler.tool_events.lock().unwrap();
            assert_eq!(events.len(), 4, "events: {events:?}");
            match &events[0] {
                ToolEvent::Start { name, args, .. } => {
                    assert_eq!(name, "bash");
                    assert_eq!(args["cmd"], "echo hi");
                }
                other => panic!("expected bash start, got {other:?}"),
            }
            match &events[1] {
                ToolEvent::End {
                    name,
                    result,
                    error,
                    ..
                } => {
                    assert_eq!(name, "bash");
                    assert!(result.contains("hi"), "result: {result}");
                    assert!(error.is_none());
                }
                other => panic!("expected bash end, got {other:?}"),
            }
            match &events[3] {
                ToolEvent::End { name, error, .. } => {
                    assert_eq!(name, "edit_file");
                    assert!(error.is_some(), "failures mark the end event: {events:?}");
                }
                other => panic!("expected edit_file end, got {other:?}"),
            }
        }
        repl.shutdown().await;
    }

    #[tokio::test]
    async fn fusion_helper_fans_out_and_synthesizes() {
        let handler = Arc::new(FusionMock::default());
        let (mut repl, handler) = started_with(handler, EnvSetup::default()).await;
        let r = repl
            .exec(
                r#"
result = fusion(
    "Should we ship it?",
    analysis_models=["anthropic/model-a", "openai/model-b"],
    judge_model="judge/model-c",
)
print(result["final_answer"])
print(result["panel"][0]["model"])
print(result["panel"][1]["model"])
print(result["synthesis"]["consensus"][0])
"#,
                EXEC_TIMEOUT,
            )
            .await
            .unwrap();
        assert!(
            r.stdout.contains("merged final answer"),
            "stdout: {}",
            r.stdout
        );
        assert!(
            r.stdout.contains("anthropic/model-a"),
            "stdout: {}",
            r.stdout
        );
        assert!(r.stdout.contains("openai/model-b"), "stdout: {}", r.stdout);
        assert!(
            r.stdout.contains("shared conclusion"),
            "stdout: {}",
            r.stdout
        );

        {
            let seen = handler.seen.lock().unwrap();
            assert_eq!(seen.len(), 3, "seen: {seen:?}");
            assert!(
                seen.iter()
                    .any(|(_, _, m)| m.as_deref() == Some("anthropic/model-a")),
                "seen: {seen:?}"
            );
            assert!(
                seen.iter()
                    .any(|(_, _, m)| m.as_deref() == Some("openai/model-b")),
                "seen: {seen:?}"
            );
            assert!(
                seen.iter()
                    .any(|(_, _, m)| m.as_deref() == Some("judge/model-c")),
                "seen: {seen:?}"
            );
        }
        repl.shutdown().await;
    }

    #[tokio::test]
    async fn fusion_helper_can_use_recursive_queries() {
        let handler = Arc::new(FusionMock::default());
        let (mut repl, handler) = started_with(handler, EnvSetup::default()).await;
        let r = repl
            .exec(
                r#"
result = fusion(
    "Dig deeper",
    analysis_models=["panel/model"],
    judge_model="judge/model",
    recursive=True,
)
print(result["final_answer"])
print(result["synthesis"]["consensus"][0])
"#,
                EXEC_TIMEOUT,
            )
            .await
            .unwrap();
        assert!(
            r.stdout.contains("recursive final answer"),
            "stdout: {}",
            r.stdout
        );
        assert!(
            r.stdout.contains("recursive consensus"),
            "stdout: {}",
            r.stdout
        );

        {
            let seen = handler.seen.lock().unwrap();
            assert!(
                seen.iter().all(|(kind, _, _)| kind == "rlm"),
                "seen: {seen:?}"
            );
        }
        repl.shutdown().await;
    }

    #[tokio::test]
    async fn prelude_internals_hidden_from_vars() {
        let (mut repl, _events) = started().await;
        let r = repl.exec("x = 1", EXEC_TIMEOUT).await.unwrap();
        assert!(r.vars.contains(&"x".to_owned()));
        assert!(
            !r.vars.iter().any(|v| v.starts_with('_')),
            "vars: {:?}",
            r.vars
        );
        assert!(!r.vars.contains(&"bash".to_owned()), "vars: {:?}", r.vars);
        repl.shutdown().await;
    }
}
