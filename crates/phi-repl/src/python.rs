use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, Semaphore, oneshot};
use tokio::task::JoinHandle;

use crate::protocol::DriverFrame;
use crate::{
    AnswerState, EnvSetup, ReplEnv, ReplError, ReplResult, SubcallHandler, SubcallRequest,
    SubcallResponse, ToolEvent,
};

const DRIVER: &str = concat!(
    include_str!("builtins.py"),
    include_str!("jobs.py"),
    include_str!("router.py"),
);
const SETUP_TIMEOUT: Duration = Duration::from_secs(30);
const ANSWER_STATE_TIMEOUT: Duration = Duration::from_secs(10);
/// Extra wait beyond the driver's soft timeout. The driver backgrounds
/// long-running code and replies within the soft timeout on its own; the
/// hard timeout only fires when the driver itself is wedged, and that path
/// still kills and respawns the interpreter.
const EXEC_GRACE: Duration = Duration::from_secs(15);
const STDERR_CAP: usize = 64 * 1024;

type PendingMap = Arc<Mutex<std::collections::HashMap<u64, oneshot::Sender<DriverFrame>>>>;

pub struct PythonRepl {
    python: String,
    handler: Arc<dyn SubcallHandler>,
    setup: Option<EnvSetup>,
    proc: Option<Proc>,
    next_id: u64,
}

struct Proc {
    child: Child,
    stdin: Arc<Mutex<ChildStdin>>,
    pending: PendingMap,
    stderr: Arc<std::sync::Mutex<String>>,
    _semaphore: Arc<Semaphore>,
    _reader: JoinHandle<()>,
    _driver: tempfile::TempPath,
}

impl PythonRepl {
    pub fn new(handler: Arc<dyn SubcallHandler>) -> Self {
        Self::with_python("python3", handler)
    }

    pub fn with_python(python: impl Into<String>, handler: Arc<dyn SubcallHandler>) -> Self {
        Self {
            python: python.into(),
            handler,
            setup: None,
            proc: None,
            next_id: 0,
        }
    }

    async fn send(stdin: &Arc<Mutex<ChildStdin>>, frame: &Value) -> std::io::Result<()> {
        let mut line = frame.to_string();
        line.push('\n');
        let mut guard = stdin.lock().await;
        guard.write_all(line.as_bytes()).await?;
        guard.flush().await
    }

    async fn spawn(&mut self) -> Result<(), ReplError> {
        let setup = self.setup.clone().ok_or(ReplError::NotStarted)?;

        let mut driver = tempfile::Builder::new()
            .prefix("phi-repl-driver-")
            .suffix(".py")
            .tempfile()?;
        use std::io::Write;
        driver.write_all(DRIVER.as_bytes())?;
        driver.flush()?;
        let driver = driver.into_temp_path();

        let mut child = Command::new(&self.python)
            .arg(&driver)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;

        let stdin = Arc::new(Mutex::new(child.stdin.take().expect("piped stdin")));
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr_pipe = child.stderr.take().expect("piped stderr");
        let stderr = Arc::new(std::sync::Mutex::new(String::new()));
        {
            let stderr = stderr.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr_pipe).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let mut buf = stderr.lock().unwrap();
                    if buf.len() < STDERR_CAP {
                        buf.push_str(&line);
                        buf.push('\n');
                    }
                }
            });
        }

        let pending: PendingMap = Arc::new(Mutex::new(std::collections::HashMap::new()));
        let semaphore = Arc::new(Semaphore::new(setup.max_concurrent_subcalls.max(1)));
        let handler = self.handler.clone();
        let stdin_for_reader = stdin.clone();
        let pending_for_reader = pending.clone();
        let semaphore_for_reader = semaphore.clone();

        let reader = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        if line.trim().is_empty() {
                            continue;
                        }
                        let frame = match serde_json::from_str::<DriverFrame>(&line) {
                            Ok(f) => f,
                            Err(_) => continue,
                        };
                        match frame {
                            DriverFrame::Subcall {
                                id,
                                kind,
                                prompt,
                                model,
                                name,
                                args,
                                kwargs,
                                preamble,
                            } => {
                                let request = match kind.as_str() {
                                    "llm" => SubcallRequest::Llm {
                                        prompt: prompt.unwrap_or_default(),
                                        model,
                                    },
                                    "rlm" => SubcallRequest::Rlm {
                                        prompt: prompt.unwrap_or_default(),
                                        model,
                                        preamble,
                                    },
                                    "fn" => SubcallRequest::HostFn {
                                        name: name.unwrap_or_default(),
                                        args: args.unwrap_or_else(|| Value::Array(Vec::new())),
                                        kwargs: kwargs.unwrap_or_else(|| json!({})),
                                    },
                                    "ext_tool" => SubcallRequest::ExtTool {
                                        name: match name {
                                            Some(n) => n,
                                            None => {
                                                let frame = json!({"type": "subcall_result", "id": id, "error": "ext_tool missing name"});
                                                let _ = Self::send(&stdin_for_reader, &frame).await;
                                                continue;
                                            }
                                        },
                                        args: args
                                            .unwrap_or_else(|| Value::Object(Default::default())),
                                    },
                                    other => {
                                        let frame = json!({"type": "subcall_result", "id": id, "error": format!("unknown subcall kind: {other}")});
                                        let _ = Self::send(&stdin_for_reader, &frame).await;
                                        continue;
                                    }
                                };
                                let handler = handler.clone();
                                let stdin = stdin_for_reader.clone();
                                let semaphore = semaphore_for_reader.clone();
                                tokio::spawn(async move {
                                    let _permit = semaphore.acquire_owned().await;
                                    let frame = match handler.handle(request).await {
                                        Ok(SubcallResponse::Text(text)) => {
                                            json!({"type": "subcall_result", "id": id, "text": text})
                                        }
                                        // Json and Value both land in the "value" field so
                                        // the Python side receives the dict/value verbatim.
                                        Ok(SubcallResponse::Json(value))
                                        | Ok(SubcallResponse::Value(value)) => {
                                            json!({"type": "subcall_result", "id": id, "value": value})
                                        }
                                        Err(error) => {
                                            json!({"type": "subcall_result", "id": id, "error": error})
                                        }
                                    };
                                    let _ = Self::send(&stdin, &frame).await;
                                });
                            }
                            DriverFrame::ToolEvent {
                                phase,
                                name,
                                call_id,
                                args,
                                result,
                                error,
                                duration,
                            } => {
                                let event = match phase.as_str() {
                                    "start" => ToolEvent::Start {
                                        call_id,
                                        name,
                                        args: args.unwrap_or_else(|| json!({})),
                                    },
                                    "end" => ToolEvent::End {
                                        call_id,
                                        name,
                                        result: result.unwrap_or_default(),
                                        error,
                                        duration: Duration::from_secs_f64(duration.unwrap_or(0.0)),
                                    },
                                    _ => continue,
                                };
                                handler.notify_tool(event);
                            }
                            DriverFrame::Message { text } => {
                                handler.notify_message(text);
                            }
                            DriverFrame::ExecResult { id, .. }
                            | DriverFrame::AnswerState { id, .. } => {
                                let mut p = pending_for_reader.lock().await;
                                if let Some(tx) = p.remove(&id) {
                                    let _ = tx.send(frame);
                                }
                            }
                            DriverFrame::Ready { .. } => {
                                let mut p = pending_for_reader.lock().await;
                                // Ready frame: deliver to the startup waiter (id=0)
                                if let Some(tx) = p.remove(&0u64) {
                                    let _ = tx.send(frame);
                                }
                            }
                            // Fatal handled silently in reader to avoid double-respawn panics
                            DriverFrame::Fatal { .. } => {}
                        }
                    }
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
            // Drain any pending waiters on EOF/Error
            let mut p = pending_for_reader.lock().await;
            for (_, tx) in p.drain() {
                let _ = tx.send(DriverFrame::Fatal {
                    error: "driver exited".into(),
                });
            }
        });

        let proc = Proc {
            child,
            stdin,
            pending,
            stderr,
            _semaphore: semaphore,
            _reader: reader,
            _driver: driver,
        };

        let frame = json!({
            "type": "setup",
            "context": setup.context,
            "context_str": setup.context_str,
            "setup_code": setup.setup_code,
            "cwd": setup.cwd,
            "host_fns": setup.host_fns,
            "max_concurrent_subcalls": setup.max_concurrent_subcalls,
            "max_output_chars": setup.max_output_chars,
        });
        Self::send(&proc.stdin, &frame).await?;

        let (tx, rx) = oneshot::channel::<DriverFrame>();
        proc.pending.lock().await.insert(0, tx);

        let ready = tokio::time::timeout(SETUP_TIMEOUT, rx).await;
        let ready = ready
            .map_err(|_| ReplError::Timeout(SETUP_TIMEOUT))?
            .map_err(|_| ReplError::Protocol("driver dropped ready sender".into()))?;

        if let DriverFrame::Ready { error: Some(error) } = ready {
            return Err(ReplError::Protocol(format!("setup failed: {error}")));
        }

        self.proc = Some(proc);
        Ok(())
    }

    async fn kill_current(&mut self) {
        if let Some(mut proc) = self.proc.take() {
            let _ = proc.child.start_kill();
            let _ = proc.child.wait().await;
        }
    }

    async fn respawn(&mut self) {
        self.kill_current().await;
        if self.spawn().await.is_err() {
            self.proc = None;
        }
    }

    async fn wait_frame(
        &mut self,
        id: u64,
        timeout: Duration,
        frame_kind: &str,
    ) -> Result<DriverFrame, ReplError> {
        let proc = self.proc.as_mut().ok_or(ReplError::NotStarted)?;
        let (tx, rx) = oneshot::channel::<DriverFrame>();
        proc.pending.lock().await.insert(id, tx);

        let result = tokio::time::timeout(timeout, rx).await;
        match result {
            Ok(Ok(frame)) => {
                if frame_kind == "exec"
                    && let DriverFrame::Fatal { error } = &frame
                {
                    let stderr_msg = {
                        let buf = proc.stderr.lock().unwrap();
                        format!("{}: {}", error, buf.trim())
                    };
                    self.respawn().await;
                    return Err(ReplError::Crashed(stderr_msg));
                }
                Ok(frame)
            }
            Ok(Err(_)) => {
                let stderr_msg = self
                    .proc
                    .as_ref()
                    .map(|p| {
                        let buf = p.stderr.lock().unwrap();
                        format!("driver exited unexpectedly: {}", buf.trim())
                    })
                    .unwrap_or_else(|| "driver exited unexpectedly".to_owned());
                self.respawn().await;
                Err(ReplError::Protocol(stderr_msg))
            }
            Err(_) => {
                // Remove the pending entry on timeout to avoid leaks
                if let Some(proc) = self.proc.as_ref() {
                    proc.pending.lock().await.remove(&id);
                }
                self.respawn().await;
                Err(ReplError::Timeout(timeout))
            }
        }
    }
}

#[async_trait]
impl ReplEnv for PythonRepl {
    async fn start(&mut self, setup: EnvSetup) -> Result<(), ReplError> {
        self.setup = Some(setup);
        self.kill_current().await;
        self.spawn().await
    }

    async fn exec(&mut self, code: &str, timeout: Duration) -> Result<ReplResult, ReplError> {
        let start = Instant::now();
        self.next_id += 1;
        let id = self.next_id;
        let proc = self.proc.as_mut().ok_or(ReplError::NotStarted)?;

        if let Err(e) = Self::send(
            &proc.stdin,
            &json!({"type": "exec", "id": id, "code": code, "timeout": timeout.as_secs_f64()}),
        )
        .await
        {
            let stderr_msg = {
                let buf = proc.stderr.lock().unwrap();
                format!("{e}: {}", buf.trim())
            };
            self.respawn().await;
            return Err(ReplError::Crashed(stderr_msg));
        }

        let wait_timeout = timeout + EXEC_GRACE;
        let frame = self.wait_frame(id, wait_timeout, "exec").await?;

        match frame {
            DriverFrame::ExecResult {
                stdout,
                stderr,
                vars,
                final_answer,
                final_summary,
                truncated,
                ..
            } => Ok(ReplResult {
                stdout,
                stderr,
                vars,
                final_answer,
                final_summary,
                duration: start.elapsed(),
                truncated,
            }),
            _ => Err(ReplError::Protocol("unexpected frame for exec".into())),
        }
    }

    async fn answer_state(&mut self) -> Result<AnswerState, ReplError> {
        self.next_id += 1;
        let id = self.next_id;
        let proc = self.proc.as_mut().ok_or(ReplError::NotStarted)?;
        Self::send(&proc.stdin, &json!({"type": "answer_state", "id": id})).await?;
        let frame = self.wait_frame(id, ANSWER_STATE_TIMEOUT, "answer").await?;
        match frame {
            DriverFrame::AnswerState { content, ready, .. } => Ok(AnswerState { content, ready }),
            _ => Err(ReplError::Protocol(
                "unexpected frame for answer_state".into(),
            )),
        }
    }

    async fn shutdown(&mut self) {
        if let Some(mut proc) = self.proc.take() {
            let _ = Self::send(&proc.stdin, &json!({"type": "shutdown"})).await;
            if tokio::time::timeout(Duration::from_secs(2), proc.child.wait())
                .await
                .is_err()
            {
                let _ = proc.child.start_kill();
                let _ = proc.child.wait().await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestHandler;

    #[async_trait]
    impl SubcallHandler for TestHandler {
        async fn handle(&self, request: SubcallRequest) -> Result<SubcallResponse, String> {
            match request {
                SubcallRequest::Llm { prompt, .. } => {
                    Ok(SubcallResponse::Text(format!("llm:{prompt}")))
                }
                SubcallRequest::Rlm { prompt, .. } => {
                    Ok(SubcallResponse::Text(format!("rlm:{prompt}")))
                }
                SubcallRequest::HostFn { name, args, .. } => match name.as_str() {
                    "add" => {
                        let sum: i64 = args
                            .as_array()
                            .map(|a| a.iter().filter_map(Value::as_i64).sum())
                            .unwrap_or(0);
                        Ok(SubcallResponse::Value(json!(sum)))
                    }
                    "boom" => Err("host fn exploded".into()),
                    other => Err(format!("unknown fn: {other}")),
                },
                SubcallRequest::ExtTool { name, .. } => Err(format!("no tool named '{name}'")),
            }
        }
    }

    async fn started(setup: EnvSetup) -> PythonRepl {
        let mut repl = PythonRepl::new(Arc::new(TestHandler));
        repl.start(setup).await.expect("start");
        repl
    }

    const EXEC_TIMEOUT: Duration = Duration::from_secs(20);

    #[tokio::test]
    async fn persists_namespace_across_execs() {
        let mut repl = started(EnvSetup::default()).await;
        let r = repl.exec("x = 41", EXEC_TIMEOUT).await.unwrap();
        assert_eq!(r.stdout, "");
        assert!(r.vars.contains(&"x".to_owned()));
        let r = repl.exec("print(x + 1)", EXEC_TIMEOUT).await.unwrap();
        assert_eq!(r.stdout, "42\n");
        repl.shutdown().await;
    }

    #[tokio::test]
    async fn answer_protocol_round_trips() {
        let mut repl = started(EnvSetup::default()).await;
        let r = repl
            .exec("answer['content'] = 'done'", EXEC_TIMEOUT)
            .await
            .unwrap();
        assert_eq!(r.final_answer, None);
        let state = repl.answer_state().await.unwrap();
        assert!(!state.ready);
        let r = repl
            .exec("answer['ready'] = True", EXEC_TIMEOUT)
            .await
            .unwrap();
        assert_eq!(r.final_answer.as_deref(), Some("done"));
        let state = repl.answer_state().await.unwrap();
        assert!(state.ready);
        assert_eq!(state.content, "done");
        repl.shutdown().await;
    }

    #[tokio::test]
    async fn answer_summary_crosses_on_every_exec() {
        let mut repl = started(EnvSetup::default()).await;
        let r = repl.exec("x = 1", EXEC_TIMEOUT).await.unwrap();
        assert_eq!(r.final_summary, None);
        // A summary is a standing declaration: reported from the exec that
        // sets it onward, independent of answer['ready'].
        let r = repl
            .exec("answer['summary'] = 'did the thing'", EXEC_TIMEOUT)
            .await
            .unwrap();
        assert_eq!(r.final_summary.as_deref(), Some("did the thing"));
        let r = repl.exec("y = 2", EXEC_TIMEOUT).await.unwrap();
        assert_eq!(r.final_summary.as_deref(), Some("did the thing"));
        // Whitespace-only summaries are treated as unset.
        let r = repl
            .exec("answer['summary'] = '  '", EXEC_TIMEOUT)
            .await
            .unwrap();
        assert_eq!(r.final_summary, None);
        repl.shutdown().await;
    }

    #[tokio::test]
    async fn llm_query_round_trips() {
        let mut repl = started(EnvSetup::default()).await;
        let r = repl
            .exec("print(llm_query('hi'))", EXEC_TIMEOUT)
            .await
            .unwrap();
        assert_eq!(r.stdout, "llm:hi\n");
        let r = repl
            .exec("print(subagent('deep'))", EXEC_TIMEOUT)
            .await
            .unwrap();
        assert_eq!(r.stdout, "rlm:deep\n");
        repl.shutdown().await;
    }

    #[tokio::test]
    async fn batched_queries_preserve_order() {
        let mut repl = started(EnvSetup::default()).await;
        let r = repl
            .exec(
                "print(llm_query_batched(['a', 'b', 'c', 'd', 'e']))",
                EXEC_TIMEOUT,
            )
            .await
            .unwrap();
        assert_eq!(r.stdout, "['llm:a', 'llm:b', 'llm:c', 'llm:d', 'llm:e']\n");
        repl.shutdown().await;
    }

    #[tokio::test]
    async fn host_fns_round_trip_and_raise() {
        let setup = EnvSetup {
            host_fns: vec!["add".into(), "boom".into()],
            ..EnvSetup::default()
        };
        let mut repl = started(setup).await;
        let r = repl.exec("print(add(2, 3))", EXEC_TIMEOUT).await.unwrap();
        assert_eq!(r.stdout, "5\n");
        let r = repl.exec("boom()", EXEC_TIMEOUT).await.unwrap();
        assert!(
            r.stderr.contains("RuntimeError: host fn exploded"),
            "{}",
            r.stderr
        );
        repl.shutdown().await;
    }

    #[tokio::test]
    async fn errors_surface_in_stderr_and_env_survives() {
        let mut repl = started(EnvSetup::default()).await;
        let r = repl.exec("missing_var", EXEC_TIMEOUT).await.unwrap();
        assert!(r.stderr.contains("NameError"), "{}", r.stderr);
        let r = repl.exec("print('ok')", EXEC_TIMEOUT).await.unwrap();
        assert_eq!(r.stdout, "ok\n");
        repl.shutdown().await;
    }

    #[tokio::test]
    async fn soft_timeout_backgrounds_long_exec() {
        let mut repl = started(EnvSetup::default()).await;
        let r = repl
            .exec(
                "import time\nprint('starting')\ntime.sleep(30)",
                Duration::from_millis(500),
            )
            .await
            .unwrap();
        assert!(r.stdout.contains("starting"), "{}", r.stdout);
        assert!(r.stdout.contains("background job 1"), "{}", r.stdout);
        // The interpreter survives: namespace intact, no respawn.
        let r = repl
            .exec("x = 7\nprint('alive')", EXEC_TIMEOUT)
            .await
            .unwrap();
        assert_eq!(r.stdout, "alive\n");
        let r = repl.exec("print(x)", EXEC_TIMEOUT).await.unwrap();
        assert_eq!(r.stdout, "7\n");
        let r = repl
            .exec("print(job_status(1))", EXEC_TIMEOUT)
            .await
            .unwrap();
        assert!(r.stdout.contains("running"), "{}", r.stdout);
        repl.shutdown().await;
    }

    #[tokio::test]
    async fn bg_jobs_run_and_write_back() {
        let mut repl = started(EnvSetup::default()).await;
        let r = repl
            .exec(
                "jid = bg(\"import time\\ntime.sleep(0.3)\\nx = 99\\nprint('marker')\")\nprint(jid > 0)",
                EXEC_TIMEOUT,
            )
            .await
            .unwrap();
        assert_eq!(r.stdout, "True\n");

        let mut status = String::new();
        for _ in 0..50 {
            let r = repl
                .exec("print(job_status(jid))", EXEC_TIMEOUT)
                .await
                .unwrap();
            status = r.stdout;
            if status.contains("[done") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(status.contains("[done"), "{status}");

        let r = repl
            .exec("print(job_output(jid))", EXEC_TIMEOUT)
            .await
            .unwrap();
        assert!(r.stdout.contains("marker"), "{}", r.stdout);
        // Variables assigned by the job land in the shared namespace.
        let r = repl.exec("print(x)", EXEC_TIMEOUT).await.unwrap();
        assert_eq!(r.stdout, "99\n");
        repl.shutdown().await;
    }

    #[tokio::test]
    async fn job_output_isolated_from_foreground() {
        let mut repl = started(EnvSetup::default()).await;
        let r = repl
            .exec(
                "jid = bg(\"import time\\ntime.sleep(0.5)\\nprint('from bg')\")\nprint('fg only')",
                EXEC_TIMEOUT,
            )
            .await
            .unwrap();
        assert!(r.stdout.contains("fg only"), "{}", r.stdout);
        assert!(!r.stdout.contains("from bg"), "{}", r.stdout);
        repl.shutdown().await;
    }

    #[tokio::test]
    async fn output_is_truncated() {
        let setup = EnvSetup {
            max_output_chars: 1000,
            ..EnvSetup::default()
        };
        let mut repl = started(setup).await;
        let r = repl.exec("print('a' * 5000)", EXEC_TIMEOUT).await.unwrap();
        assert!(r.truncated);
        assert!(r.stdout.starts_with(&"a".repeat(1000)));
        assert!(r.stdout.contains("chars...]"), "{}", r.stdout);
        repl.shutdown().await;
    }

    #[tokio::test]
    async fn context_loads_and_scaffold_restores() {
        let setup = EnvSetup {
            context_str: Some("hello".into()),
            ..EnvSetup::default()
        };
        let mut repl = started(setup).await;
        let r = repl
            .exec("print(len(context))", EXEC_TIMEOUT)
            .await
            .unwrap();
        assert_eq!(r.stdout, "5\n");
        repl.exec("context = 'clobbered'", EXEC_TIMEOUT)
            .await
            .unwrap();
        let r = repl.exec("print(context)", EXEC_TIMEOUT).await.unwrap();
        assert_eq!(r.stdout, "hello\n");
        let r = repl.exec("print(SHOW_VARS())", EXEC_TIMEOUT).await.unwrap();
        assert!(r.stdout.contains("context_0"), "{}", r.stdout);
        repl.shutdown().await;
    }

    #[tokio::test]
    async fn setup_code_runs_before_first_exec() {
        let setup = EnvSetup {
            setup_code: Some("base = 10".into()),
            ..EnvSetup::default()
        };
        let mut repl = started(setup).await;
        let r = repl.exec("print(base * 2)", EXEC_TIMEOUT).await.unwrap();
        assert_eq!(r.stdout, "20\n");
        repl.shutdown().await;
    }
}
