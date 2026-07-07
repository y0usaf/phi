//! bash — run a shell command in its own process group (killable as a tree
//! on abort/timeout), tail-truncated output with spill-to-file.

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::truncate;

/// How often the wait loop polls for exit / cancel / timeout.
const POLL: Duration = Duration::from_millis(25);

pub fn register(host: &phi_host::Host, cancel: CancellationToken) -> anyhow::Result<()> {
    host.register_tool(
        "bash",
        "Execute a bash command and return its combined stdout/stderr. \
         Use for builds, tests, git, and file operations like ls, rg, find.",
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The command to run with bash -c" },
                "timeout": { "type": "integer", "description": "Seconds before the command is killed (no default timeout)" }
            },
            "required": ["command"]
        }),
        super::SOURCE,
        Box::new(move |args| execute(args, &cancel)),
    )
}

enum Ending {
    Exited(std::process::ExitStatus),
    TimedOut(u64),
    Aborted,
}

fn execute(args: &serde_json::Value, cancel: &CancellationToken) -> Result<String, String> {
    let command = super::require_str(args, "command")?;
    let timeout = args.get("timeout").and_then(|v| v.as_u64());

    let mut child = spawn(command).map_err(|e| format!("spawn bash: {e}"))?;
    let buf: Arc<Mutex<Vec<u8>>> = Arc::default();
    let stdout: Option<Box<dyn Read + Send>> = child
        .stdout
        .take()
        .map(|p| Box::new(p) as Box<dyn Read + Send>);
    let stderr: Option<Box<dyn Read + Send>> = child
        .stderr
        .take()
        .map(|p| Box::new(p) as Box<dyn Read + Send>);
    let readers = [stdout, stderr].map(|pipe| {
        let buf = Arc::clone(&buf);
        pipe.map(|mut pipe| {
            std::thread::spawn(move || {
                let mut chunk = [0u8; 8192];
                while let Ok(n) = pipe.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    buf.lock().expect("bash buf").extend_from_slice(&chunk[..n]);
                }
            })
        })
    });

    let start = Instant::now();
    let ending = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ending::Exited(status),
            Ok(None) => {}
            Err(e) => {
                kill_group(&child);
                return Err(format!("wait on bash: {e}"));
            }
        }
        if cancel.is_cancelled() {
            kill_group(&child);
            let _ = child.wait();
            break Ending::Aborted;
        }
        if let Some(secs) = timeout
            && start.elapsed() >= Duration::from_secs(secs)
        {
            kill_group(&child);
            let _ = child.wait();
            break Ending::TimedOut(secs);
        }
        std::thread::sleep(POLL);
    };
    for reader in readers.into_iter().flatten() {
        let _ = reader.join();
    }

    let raw = buf.lock().expect("bash buf").clone();
    let output = String::from_utf8_lossy(&raw).into_owned();
    let t = truncate::tail(&output, truncate::MAX_LINES, truncate::MAX_BYTES);
    let mut text = t.text;
    if t.truncated {
        let note = match spill(&output) {
            Some(path) => format!(
                "[Output truncated: showing the last {} of {} lines. Full output: {}]\n",
                t.kept,
                t.total,
                path.display()
            ),
            None => format!(
                "[Output truncated: showing the last {} of {} lines.]\n",
                t.kept, t.total
            ),
        };
        text.insert_str(0, &note);
    }

    match ending {
        Ending::Exited(status) if status.success() => Ok(if text.is_empty() {
            "(no output)".to_owned()
        } else {
            text
        }),
        Ending::Exited(status) => Err(match status.code() {
            Some(code) => format!("Exit code {code}\n{text}"),
            None => format!("Killed by signal\n{text}"),
        }),
        Ending::TimedOut(secs) => Err(format!("Command timed out after {secs}s\n{text}")),
        Ending::Aborted => Err(format!("Command aborted\n{text}")),
    }
}

fn spawn(command: &str) -> std::io::Result<std::process::Child> {
    use std::os::unix::process::CommandExt as _;
    Command::new("bash")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
}

/// Kill the child's entire process group (it was made its own group leader
/// at spawn), so pipelines and grandchildren die with it.
fn kill_group(child: &std::process::Child) {
    unsafe {
        libc::killpg(child.id() as i32, libc::SIGKILL);
    }
}

/// Persist full output to a temp file the model (or user) can page through.
fn spill(output: &str) -> Option<std::path::PathBuf> {
    let path = std::env::temp_dir().join(format!(
        "phi-bash-{}-{}.txt",
        std::process::id(),
        phi_ai::now_ms()
    ));
    std::fs::write(&path, output).ok()?;
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::execute;
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    fn run(args: serde_json::Value) -> Result<String, String> {
        execute(&args, &CancellationToken::new())
    }

    #[test]
    fn captures_stdout_and_stderr() {
        let out = run(json!({ "command": "echo out; echo err >&2" })).unwrap();
        assert!(out.contains("out"), "{out}");
        assert!(out.contains("err"), "{out}");
    }

    #[test]
    fn nonzero_exit_is_an_error_with_output() {
        let err = run(json!({ "command": "echo boom; exit 3" })).unwrap_err();
        assert!(err.starts_with("Exit code 3"), "{err}");
        assert!(err.contains("boom"), "{err}");
    }

    #[test]
    fn timeout_kills_the_command() {
        let start = std::time::Instant::now();
        let err = run(json!({ "command": "sleep 30", "timeout": 1 })).unwrap_err();
        assert!(err.contains("timed out"), "{err}");
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn cancel_aborts_the_command() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let err = execute(&json!({ "command": "sleep 30" }), &cancel).unwrap_err();
        assert!(err.contains("aborted"), "{err}");
    }

    #[test]
    fn no_output_is_reported() {
        assert_eq!(run(json!({ "command": "true" })).unwrap(), "(no output)");
    }

    #[test]
    fn long_output_is_tail_truncated_with_spill() {
        let out = run(json!({ "command": "seq 1 5000" })).unwrap();
        assert!(out.contains("[Output truncated"), "{out}");
        assert!(out.trim_end().ends_with("5000"), "{out}");
        assert!(!out.contains("\n1\n"), "head should be gone");
    }
}
