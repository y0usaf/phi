use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use phi_repl::{EnvSetup, PythonRepl, SubcallHandler, SubcallRequest, SubcallResponse, ToolEvent};

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

/// Subcall handler for spill tests: prompts containing "big" get a >50K-char
/// response (over the driver's SPILL_THRESHOLD), everything else stays small.
struct SizedSubcalls;

#[async_trait]
impl SubcallHandler for SizedSubcalls {
    async fn handle(&self, request: SubcallRequest) -> Result<SubcallResponse, String> {
        match request {
            SubcallRequest::Llm { prompt, .. } => {
                Ok(SubcallResponse::Text(if prompt.contains("big") {
                    "x".repeat(60_000)
                } else {
                    "small-result".to_owned()
                }))
            }
            SubcallRequest::Rlm { prompt, .. } => {
                let text = if prompt.contains("big") {
                    "y".repeat(60_000)
                } else {
                    "small-result".to_owned()
                };
                Ok(SubcallResponse::Value(serde_json::json!({
                    "text": text,
                    "stop": "answer",
                    "iterations": 1,
                    "ok": true,
                })))
            }
            _ => Err("unsupported in spill tests".to_owned()),
        }
    }
}

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
        .exec(
            &format!("print(read_file({path:?}, raw=True))"),
            EXEC_TIMEOUT,
        )
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
            &format!("read_file({path:?})\nprint(write_file({path:?}, 'alpha\\ngamma\\n'))"),
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
async fn bash_spills_large_output_to_tmp_file() {
    let (mut repl, _events) = started().await;
    // Generate output that exceeds _PRELUDE_MAX_BYTES (50_000).
    // Assign to a variable and print only the tail end: printing the
    // full spill result (50K tail + annotation) would get truncated by
    // the REPL's own stdout limit.
    let cmd = "python3 -c \"print('x' * 60000)\"";
    let r = repl
        .exec(
            &format!("result = bash({cmd:?})\nprint(result[-200:])"),
            EXEC_TIMEOUT,
        )
        .await
        .unwrap();
    // The last 200 chars of the spill result contain the annotation.
    assert!(
        r.stdout.contains("[full output: "),
        "should spill to tmp file: {}",
        r.stdout
    );
    // Extract the tmp file path and verify it exists with the full content.
    let path_start = r.stdout.find("[full output: ").unwrap() + "[full output: ".len();
    let path_end = r.stdout[path_start..]
        .find('\n')
        .map(|p| p + path_start)
        .unwrap_or(r.stdout.len());
    let tmp_path = r.stdout[path_start..path_end].trim().trim_end_matches(']');
    assert!(
        std::fs::exists(tmp_path).unwrap_or(false),
        "spilled tmp file should exist: {tmp_path}"
    );
    let content = std::fs::read_to_string(tmp_path).unwrap();
    assert_eq!(content.len(), 60001); // 60000 x's + newline
    let _ = std::fs::remove_file(tmp_path);
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
            &format!("print(grep('needle', path={root:?}, raw=True))"),
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
        .exec(&format!("print(read_file({path:?}))"), EXEC_TIMEOUT)
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
        r#"view = read_file({path:?}, fresh=True)
header = view.splitlines()[0]
patch = header + "\nSWAP 2.=2:\n+    return 'hello ' + name\n"
print(edit_file(patch=patch))"#
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

/// Guards the doc claim in `coding_tools_section()` (WS9.3c few-shot example):
/// a multi-op section mixes SWAP/INS/DEL and every line number refers to the
/// file as last read — later ops are not shifted by earlier ones.
#[tokio::test]
async fn hashline_multi_op_section_uses_as_read_line_numbers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("multi.txt");
    let path = path.to_str().unwrap();
    std::fs::write(path, "l1\nl2\nl3\nl4\nl5\nl6\nl7\n").unwrap();
    let (mut repl, _events) = started().await;

    let r = repl
        .exec(
            &format!(
                r#"view = read_file({path:?})
header = view.splitlines()[0]
patch = header + "\nSWAP 2.=3:\n+TWO\nINS.POST 5:\n+AFTER-FIVE\nDEL 6.=7\n"
print(edit_file(patch=patch))"#
            ),
            EXEC_TIMEOUT,
        )
        .await
        .unwrap();
    // SWAP shrank 2 lines to 1; INS.POST 5 and DEL 6.=7 still mean the
    // original l5/l6/l7, not the post-SWAP positions.
    assert_eq!(r.stderr, "", "stderr: {}", r.stderr);
    assert!(r.stdout.contains("1:l1"), "stdout: {}", r.stdout);
    assert!(r.stdout.contains("2:TWO"), "stdout: {}", r.stdout);
    assert!(r.stdout.contains("3:l4"), "stdout: {}", r.stdout);
    assert!(r.stdout.contains("4:l5"), "stdout: {}", r.stdout);
    assert!(r.stdout.contains("5:AFTER-FIVE"), "stdout: {}", r.stdout);
    assert!(!r.stdout.contains("l6"), "stdout: {}", r.stdout);
    assert!(!r.stdout.contains("l7"), "stdout: {}", r.stdout);
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
                r#"view = read_file({seen_path:?}, offset=0, limit=2)
header = view.splitlines()[0]
patch = header + "\nSWAP 4.=4:\n+L4\n"
print(edit_file(patch=patch))"#
            ),
            EXEC_TIMEOUT,
        )
        .await
        .unwrap();
    assert!(r.stdout.contains("unseen line(s)"), "stdout: {}", r.stdout);

    let r = repl
        .exec(
            &format!(
                r#"view = read_file({stale_path:?})
header = view.splitlines()[0]
print(edit_file(patch=header + "\nSWAP 2.=2:\n+B\n"))
print(edit_file(patch=header + "\nSWAP 3.=3:\n+C\n"))"#
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
        .exec("print(read_file('marker.txt', raw=True))", EXEC_TIMEOUT)
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

#[tokio::test]
async fn hashline_envelope_markers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("env.txt");
    let path = path.to_str().unwrap();
    std::fs::write(path, "alpha\nbeta\n").unwrap();
    let (mut repl, _events) = started().await;

    let r = repl
        .exec(
            &format!(
                r#"view = read_file({path:?})
header = view.splitlines()[0]
patch = "*** Begin Patch\n" + header + "\nSWAP 1.=1:\n+ALPHA\n*** End Patch"
print(edit_file(patch=patch))"#
            ),
            EXEC_TIMEOUT,
        )
        .await
        .unwrap();
    assert!(r.stdout.contains("1:ALPHA"), "stdout: {}", r.stdout);
    repl.shutdown().await;
}

#[tokio::test]
async fn hashline_apply_patch_contamination_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("contam.py");
    let path = path.to_str().unwrap();
    std::fs::write(path, "a\nb\nc\n").unwrap();
    let (mut repl, _events) = started().await;

    let r = repl
        .exec(
            &format!(r#"print(edit_file(patch='*** Update File: {path}\nSWAP 1.=1:\n+X'))"#),
            EXEC_TIMEOUT,
        )
        .await
        .unwrap();
    assert!(
        r.stdout.contains("apply_patch sentinel"),
        "stdout: {}",
        r.stdout
    );
    repl.shutdown().await;
}

#[tokio::test]
async fn hashline_unified_diff_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("diff.txt");
    let path = path.to_str().unwrap();
    std::fs::write(path, "line1\nline2\n").unwrap();
    let (mut repl, _events) = started().await;

    let r = repl
        .exec(
            r#"print(edit_file(patch='@@ -1,2 +1,2 @@\n-line1\n+LINE1'))"#,
            EXEC_TIMEOUT,
        )
        .await
        .unwrap();
    assert!(r.stdout.contains("unified-diff"), "stdout: {}", r.stdout);
    repl.shutdown().await;
}

#[tokio::test]
async fn hashline_lenient_range_separators() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sep.txt");
    let path = path.to_str().unwrap();
    std::fs::write(path, "a\nb\nc\nd\ne\n").unwrap();
    let (mut repl, _events) = started().await;

    let r = repl
        .exec(
            &format!(
                r#"view = read_file({path:?})
header = view.splitlines()[0]
patch = header + "\nSWAP 2..3:\n+X\n+Y"
print(edit_file(patch=patch))"#
            ),
            EXEC_TIMEOUT,
        )
        .await
        .unwrap();
    assert!(r.stdout.contains("2:X"), "stdout: {}", r.stdout);
    assert!(r.stdout.contains("3:Y"), "stdout: {}", r.stdout);
    repl.shutdown().await;
}

#[tokio::test]
async fn hashline_missing_colon_on_headers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("noc.txt");
    let path = path.to_str().unwrap();
    std::fs::write(path, "a\nb\nc\n").unwrap();
    let (mut repl, _events) = started().await;

    let r = repl
        .exec(
            &format!(
                r#"view = read_file({path:?})
header = view.splitlines()[0]
patch = header + "\nSWAP 2.=2\n+BEE"
print(edit_file(patch=patch))"#
            ),
            EXEC_TIMEOUT,
        )
        .await
        .unwrap();
    assert!(r.stdout.contains("2:BEE"), "stdout: {}", r.stdout);
    repl.shutdown().await;
}

#[tokio::test]
async fn hashline_auto_pipe_bare_body_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bare.txt");
    let path = path.to_str().unwrap();
    std::fs::write(path, "a\nb\nc\n").unwrap();
    let (mut repl, _events) = started().await;

    let r = repl
        .exec(
            &format!(
                r#"view = read_file({path:?})
header = view.splitlines()[0]
patch = header + "\nSWAP 2.=2:\nBEE"
print(edit_file(patch=patch))"#
            ),
            EXEC_TIMEOUT,
        )
        .await
        .unwrap();
    assert!(r.stdout.contains("2:BEE"), "stdout: {}", r.stdout);
    assert!(
        r.stdout.contains("Auto-prefixed"),
        "should warn about auto-pipe: {}",
        r.stdout
    );
    repl.shutdown().await;
}

#[tokio::test]
async fn hashline_strips_read_output_prefixes_in_bare_body() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("prefix.txt");
    let path = path.to_str().unwrap();
    std::fs::write(path, "aaa\nbbb\nccc\nddd\neee\n").unwrap();
    let (mut repl, _events) = started().await;

    let r = repl
        .exec(
            &format!(
                r#"view = read_file({path:?})
header = view.splitlines()[0]
patch = header + "\nSWAP 2.=4:\nline one\n2:line two"
print(edit_file(patch=patch))"#
            ),
            EXEC_TIMEOUT,
        )
        .await
        .unwrap();
    assert!(r.stdout.contains("2:line one"), "stdout: {}", r.stdout);
    assert!(
        r.stdout.contains("3:line two"),
        "should have stripped prefix: {}",
        r.stdout
    );
    repl.shutdown().await;
}

#[tokio::test]
async fn hashline_boundary_repair_drops_duplicated_closer() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("closers.txt");
    let path = path.to_str().unwrap();
    std::fs::write(
        path,
        "it('a', () => {\n\tsetup();\n\trun();\n});\nafter();\n",
    )
    .unwrap();
    let (mut repl, _events) = started().await;

    let r = repl
        .exec(
            &format!(
                r#"view = read_file({path:?})
header = view.splitlines()[0]
patch = header + "\nSWAP 2.=3:\n+\tsetup2();\n+\trun2();\n+}});"
print(edit_file(patch=patch))"#
            ),
            EXEC_TIMEOUT,
        )
        .await
        .unwrap();
    let closer_count = r.stdout.matches("});").count();
    assert_eq!(
        closer_count, 1,
        "should have exactly one closer, got {}: {}",
        closer_count, r.stdout
    );
    assert!(
        r.stdout.contains("delimiter-balance"),
        "should warn about delimiter-balance repair: {}",
        r.stdout
    );
    repl.shutdown().await;
}

#[tokio::test]
async fn hashline_rich_mismatch_message() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mismatch.txt");
    let path = path.to_str().unwrap();
    std::fs::write(path, "a\nb\nc\n").unwrap();
    let (mut repl, _events) = started().await;

    let r = repl
        .exec(
            &format!(
                r#"view = read_file({path:?})
header = view.splitlines()[0]
import builtins
with builtins.open({path:?}, 'w') as f:
    f.write('a\nMODIFIED\nc\n')
patch = header + "\nSWAP 2.=2:\n+B"
print(edit_file(patch=patch))"#
            ),
            EXEC_TIMEOUT,
        )
        .await
        .unwrap();
    assert!(
        r.stdout.contains("Recovered from stale hash tag")
            || r.stdout.contains("file changed between read and edit")
            || r.stdout.contains("stale anchor")
            || r.stdout.contains("Error:"),
        "should mention mismatch/recovery/stale: {}",
        r.stdout
    );
    repl.shutdown().await;
}

#[tokio::test]
async fn hashline_bom_preserved_on_edit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bom.txt");
    let path = path.to_str().unwrap();
    std::fs::write(path, "\u{FEFF}hello\nworld\n").unwrap();
    let (mut repl, _events) = started().await;

    let r = repl
        .exec(
            &format!(
                r#"view = read_file({path:?})
header = view.splitlines()[0]
patch = header + "\nSWAP 2.=2:\n+WORLD"
print(edit_file(patch=patch))"#
            ),
            EXEC_TIMEOUT,
        )
        .await
        .unwrap();
    assert!(r.stdout.contains("2:WORLD"), "stdout: {}", r.stdout);

    let raw = std::fs::read(path).unwrap();
    assert!(
        raw.starts_with(&[0xEF, 0xBB, 0xBF]),
        "BOM should be preserved, got: {:?}",
        &raw[..4.min(raw.len())]
    );
    repl.shutdown().await;
}

#[tokio::test]
async fn hashline_crlf_preserved_on_edit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("crlf.txt");
    let path = path.to_str().unwrap();
    std::fs::write(path, "line1\r\nline2\r\nline3\r\n").unwrap();
    let (mut repl, _events) = started().await;

    let r = repl
        .exec(
            &format!(
                r#"view = read_file({path:?})
header = view.splitlines()[0]
patch = header + "\nSWAP 2.=2:\n+REPLACED"
print(edit_file(patch=patch))"#
            ),
            EXEC_TIMEOUT,
        )
        .await
        .unwrap();
    assert!(r.stdout.contains("2:REPLACED"), "stdout: {}", r.stdout);

    let raw = std::fs::read_to_string(path).unwrap();
    assert!(
        raw.contains("\r\n"),
        "CRLF should be preserved, got: {:?}",
        raw
    );
    repl.shutdown().await;
}

#[tokio::test]
async fn hashline_atomic_write() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("atomic.txt");
    let path = path.to_str().unwrap();
    std::fs::write(path, "a\nb\nc\n").unwrap();
    let (mut repl, _events) = started().await;

    let r = repl
        .exec(
            &format!(
                r#"view = read_file({path:?})
header = view.splitlines()[0]
patch = header + "\nSWAP 1.=1:\n+A"
print(edit_file(patch=patch))"#
            ),
            EXEC_TIMEOUT,
        )
        .await
        .unwrap();
    assert!(r.stdout.contains("1:A"), "stdout: {}", r.stdout);

    let dir_listing = std::fs::read_dir(dir.path()).unwrap();
    let tmp_files: Vec<_> = dir_listing
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".phi-tmp"))
        .collect();
    assert!(
        tmp_files.is_empty(),
        "no .phi-tmp files should remain: {:?}",
        tmp_files
    );
    repl.shutdown().await;
}

#[tokio::test]
async fn hashline_minus_row_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("minus.txt");
    let path = path.to_str().unwrap();
    std::fs::write(path, "a\nb\nc\n").unwrap();
    let (mut repl, _events) = started().await;

    let r = repl
        .exec(
            &format!(
                r#"view = read_file({path:?})
header = view.splitlines()[0]
patch = header + "\nSWAP 2.=2:\n-old line\n+new line"
print(edit_file(patch=patch))"#
            ),
            EXEC_TIMEOUT,
        )
        .await
        .unwrap();
    assert!(
        r.stdout.contains("`-` rows are not valid"),
        "should reject minus rows: {}",
        r.stdout
    );
    repl.shutdown().await;
}

#[tokio::test]
async fn hashline_noop_detection() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("noop.txt");
    let path = path.to_str().unwrap();
    std::fs::write(path, "a\nb\nc\n").unwrap();
    let (mut repl, _events) = started().await;

    let r = repl
        .exec(
            &format!(
                r#"view = read_file({path:?})
header = view.splitlines()[0]
patch = header + "\nSWAP 2.=2:\n+b"
print(edit_file(patch=patch))"#
            ),
            EXEC_TIMEOUT,
        )
        .await
        .unwrap();
    assert!(
        r.stdout.contains("noop"),
        "should detect noop: {}",
        r.stdout
    );
    repl.shutdown().await;
}

// ── read-before-edit guard tests ─────────────────────────────────────────────

#[tokio::test]
async fn write_file_refuses_overwriting_unread_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("unread.txt");
    let path = path.to_str().unwrap();
    std::fs::write(path, "precious\n").unwrap();
    let (mut repl, _events) = started().await;

    let r = repl
        .exec(
            &format!("print(write_file({path:?}, 'clobbered\\n'))"),
            EXEC_TIMEOUT,
        )
        .await
        .unwrap();
    assert!(
        r.stdout.contains("has not been read in this session"),
        "stdout: {}",
        r.stdout
    );
    assert!(r.stdout.contains("read_file"), "stdout: {}", r.stdout);
    assert_eq!(std::fs::read_to_string(path).unwrap(), "precious\n");
    repl.shutdown().await;
}

#[tokio::test]
async fn edit_file_refuses_unread_and_stale_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("guarded.txt");
    let path = path.to_str().unwrap();
    std::fs::write(path, "alpha\nbeta\n").unwrap();
    let (mut repl, _events) = started().await;

    // Never read: refused with the read-first message.
    let r = repl
        .exec(
            &format!("print(edit_file({path:?}, 'alpha', 'ALPHA'))"),
            EXEC_TIMEOUT,
        )
        .await
        .unwrap();
    assert!(
        r.stdout.contains("has not been read in this session"),
        "stdout: {}",
        r.stdout
    );

    // Read, then modified externally: refused with the stale message.
    let r = repl
        .exec(
            &format!(
                r#"read_file({path:?})
import builtins
with builtins.open({path:?}, 'w') as f:
    f.write('alpha\nCHANGED\n')
print(edit_file({path:?}, 'alpha', 'ALPHA'))"#
            ),
            EXEC_TIMEOUT,
        )
        .await
        .unwrap();
    assert!(
        r.stdout.contains("changed on disk since it was last read"),
        "stdout: {}",
        r.stdout
    );
    assert!(r.stdout.contains("fresh=True"), "stdout: {}", r.stdout);
    assert_eq!(std::fs::read_to_string(path).unwrap(), "alpha\nCHANGED\n");
    repl.shutdown().await;
}

#[tokio::test]
async fn edit_guard_ignores_mtime_touch_with_identical_content() {
    // Rewriting the same bytes advances mtime but must not trip the guard
    // (content-hash matching, not mtime comparison).
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("touched.txt");
    let path = path.to_str().unwrap();
    std::fs::write(path, "alpha\nbeta\n").unwrap();
    let (mut repl, _events) = started().await;

    repl.exec(&format!("read_file({path:?})"), EXEC_TIMEOUT)
        .await
        .unwrap();
    std::fs::write(path, "alpha\nbeta\n").unwrap();

    let r = repl
        .exec(
            &format!("print(edit_file({path:?}, 'beta', 'gamma'))"),
            EXEC_TIMEOUT,
        )
        .await
        .unwrap();
    assert!(r.stdout.contains("Replaced 1"), "stdout: {}", r.stdout);
    repl.shutdown().await;
}

#[tokio::test]
async fn raw_read_satisfies_read_before_edit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("raw.txt");
    let path = path.to_str().unwrap();
    std::fs::write(path, "alpha\nbeta\n").unwrap();
    let (mut repl, _events) = started().await;

    let r = repl
        .exec(
            &format!("read_file({path:?}, raw=True)\nprint(write_file({path:?}, 'rewritten\\n'))"),
            EXEC_TIMEOUT,
        )
        .await
        .unwrap();
    assert!(r.stdout.contains("Wrote"), "stdout: {}", r.stdout);
    assert_eq!(std::fs::read_to_string(path).unwrap(), "rewritten\n");
    repl.shutdown().await;
}

// ── read_file cache tests ─────────────────────────────────────────────────────

#[tokio::test]
async fn read_file_cache_stub_on_unchanged_file() {
    // Second read of an unchanged file should return the one-line stub instead
    // of re-emitting the full content.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cached.txt");
    let path = path.to_str().unwrap();
    std::fs::write(path, "line one\nline two\nline three\n").unwrap();
    let (mut repl, _events) = started().await;

    // First read: should return full hashline-formatted content.
    let r = repl
        .exec(&format!("print(read_file({path:?}))"), EXEC_TIMEOUT)
        .await
        .unwrap();
    assert!(
        r.stdout.contains("1:line one"),
        "first read should return full content: {}",
        r.stdout
    );

    // Second read: file unchanged, should return the stub.
    let r = repl
        .exec(&format!("print(read_file({path:?}))"), EXEC_TIMEOUT)
        .await
        .unwrap();
    assert!(
        r.stdout.contains("unchanged since last read"),
        "second read of unchanged file should return stub: {}",
        r.stdout
    );
    assert!(
        !r.stdout.contains("1:line one"),
        "stub should not re-emit file content: {}",
        r.stdout
    );

    repl.shutdown().await;
}

#[tokio::test]
async fn read_file_cache_miss_after_file_modified() {
    // After the file changes on disk the cache should detect staleness via
    // mtime/size and return fresh content, not the stub.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("modified.txt");
    let path = path.to_str().unwrap();
    std::fs::write(path, "original content\n").unwrap();
    let (mut repl, _events) = started().await;

    // Prime the cache.
    let r = repl
        .exec(&format!("print(read_file({path:?}))"), EXEC_TIMEOUT)
        .await
        .unwrap();
    assert!(
        r.stdout.contains("1:original content"),
        "first read: {}",
        r.stdout
    );

    // Overwrite the file from outside the prelude so write_file does not
    // update the cache (testing external-write detection via stat).
    std::fs::write(path, "updated content\n").unwrap();

    // Second read: mtime/size differ, so full content should be returned.
    let r = repl
        .exec(&format!("print(read_file({path:?}))"), EXEC_TIMEOUT)
        .await
        .unwrap();
    assert!(
        r.stdout.contains("1:updated content"),
        "read after external write should return fresh content: {}",
        r.stdout
    );
    assert!(
        !r.stdout.contains("unchanged since last read"),
        "should not return stub after file changed: {}",
        r.stdout
    );

    repl.shutdown().await;
}

#[tokio::test]
async fn read_file_cache_fresh_bypasses_stub() {
    // fresh=True must force a full re-read even when the file is unchanged.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("forced.txt");
    let path = path.to_str().unwrap();
    std::fs::write(path, "alpha\nbeta\n").unwrap();
    let (mut repl, _events) = started().await;

    // Prime the cache.
    let r = repl
        .exec(&format!("print(read_file({path:?}))"), EXEC_TIMEOUT)
        .await
        .unwrap();
    assert!(r.stdout.contains("1:alpha"), "first read: {}", r.stdout);

    // Second read with fresh=True: should return full content, not the stub.
    let r = repl
        .exec(
            &format!("print(read_file({path:?}, fresh=True))"),
            EXEC_TIMEOUT,
        )
        .await
        .unwrap();
    assert!(
        r.stdout.contains("1:alpha"),
        "fresh=True should return full content: {}",
        r.stdout
    );
    assert!(
        !r.stdout.contains("unchanged since last read"),
        "fresh=True must not return stub: {}",
        r.stdout
    );

    repl.shutdown().await;
}

// ── WS4.3: subcall result spill ─────────────────────────────────────────────

#[tokio::test]
async fn llm_query_spills_oversized_results() {
    let handler = Arc::new(SizedSubcalls);
    let (mut repl, _h) = started_with(handler, EnvSetup::default()).await;

    // Small results stay plain strings.
    let r = repl
        .exec(
            "r = llm_query('small please')\nprint(type(r).__name__, r)",
            EXEC_TIMEOUT,
        )
        .await
        .unwrap();
    assert_eq!(r.stdout.trim(), "str small-result", "stdout: {}", r.stdout);

    // Oversized results spill: printing shows a preview + tmp path, while
    // full programmatic access (len, slicing, full_result, the file) remains.
    let r = repl
        .exec(
            "r = llm_query('big please')\nprint(len(r))\nprint(r)\nprint('---')\nprint(r.full_result == 'x' * 60000)\nprint(r[:5])\nprint(open(r.tmp_path).read() == 'x' * 60000)",
            EXEC_TIMEOUT,
        )
        .await
        .unwrap();
    assert!(r.stdout.starts_with("60000\n"), "stdout: {}", r.stdout);
    assert!(r.stdout.contains("[Full output: "), "stdout: {}", r.stdout);
    assert!(r.stdout.contains("+ [59500 chars]"), "stdout: {}", r.stdout);
    assert!(
        r.stdout.contains("True\nxxxxx\nTrue"),
        "stdout: {}",
        r.stdout
    );
    // The preview form must be small even though the payload was 60K.
    assert!(
        r.stdout.len() < 2_000,
        "stdout not spilled: {} chars",
        r.stdout.len()
    );
    repl.shutdown().await;
}

#[tokio::test]
async fn subagent_spills_oversized_text_field() {
    let handler = Arc::new(SizedSubcalls);
    let (mut repl, _h) = started_with(handler, EnvSetup::default()).await;

    let r = repl
        .exec(
            "r = subagent('big please')\nprint(r['ok'], r['stop'], len(r['text']))\nprint('spilled' if 'Full output' in str(r['text']) else 'inline')\nprint(r['text'].full_result == 'y' * 60000)",
            EXEC_TIMEOUT,
        )
        .await
        .unwrap();
    assert!(
        r.stdout.contains("True answer 60000"),
        "stdout: {}",
        r.stdout
    );
    assert!(r.stdout.contains("spilled"), "stdout: {}", r.stdout);
    assert!(r.stdout.contains("True\n"), "stdout: {}", r.stdout);

    // A spilled result submitted as the final answer must carry the full
    // text, not the preview-with-path.
    let r = repl
        .exec(
            "answer['content'] = r['text']\nanswer['ready'] = True",
            EXEC_TIMEOUT,
        )
        .await
        .unwrap();
    let final_answer = r.final_answer.expect("answer must be captured");
    assert_eq!(final_answer.len(), 60_000);
    assert!(!final_answer.contains("Full output"));
    repl.shutdown().await;
}

// ── WS4.4: scaffold restoration ─────────────────────────────────────────────

#[tokio::test]
async fn scaffold_restores_prelude_tools_and_context_after_overwrite() {
    let handler = Arc::new(NoSubcalls::default());
    let setup = EnvSetup {
        context: Some(serde_json::json!("the real context")),
        ..EnvSetup::default()
    };
    let (mut repl, _h) = started_with(handler, setup).await;

    // Clobber prelude tools, the context alias AND context_0 itself, and
    // mutate the shared builtins dict in place. Model-created variables must
    // still persist across the restore.
    let r = repl
        .exec(
            "read_file = None\nbash = 'broken'\ncontext = 'clobbered'\ncontext_0 = 'also clobbered'\n__builtins__['len'] = None\nmyvar = 41",
            EXEC_TIMEOUT,
        )
        .await
        .unwrap();
    assert_eq!(r.stderr, "", "stderr: {}", r.stderr);

    let r = repl
        .exec(
            "print(callable(read_file), callable(bash))\nprint(context)\nprint(len('abc'))\nprint(myvar + 1)",
            EXEC_TIMEOUT,
        )
        .await
        .unwrap();
    assert_eq!(r.stderr, "", "stderr: {}", r.stderr);
    assert_eq!(
        r.stdout, "True True\nthe real context\n3\n42\n",
        "stdout: {}",
        r.stdout
    );
    repl.shutdown().await;
}
