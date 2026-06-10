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

All helpers return strings (print them to inspect). Output is truncated to 2000 lines / 50KB.
These run on the user's real machine in the current working directory — edits and commands have real effects.
For long-running work (builds, test suites, servers) prefer a background job, e.g. `jid = bg(\"out = bash('cargo build', timeout=3600)\")`, then poll `job_status(jid)` / `job_output(jid)` on later turns instead of blocking this one."
        .to_owned()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use async_trait::async_trait;
    use phi_repl::{
        EnvSetup, PythonRepl, ReplEnv, SubcallHandler, SubcallRequest, SubcallResponse,
    };

    use super::CODING_PRELUDE;

    struct NoSubcalls;

    #[async_trait]
    impl SubcallHandler for NoSubcalls {
        async fn handle(&self, _request: SubcallRequest) -> Result<SubcallResponse, String> {
            Err("no subcalls in prelude tests".to_owned())
        }
    }

    const EXEC_TIMEOUT: Duration = Duration::from_secs(20);

    async fn started() -> PythonRepl {
        let mut repl = PythonRepl::new(Arc::new(NoSubcalls));
        repl.start(EnvSetup {
            setup_code: Some(CODING_PRELUDE.to_owned()),
            ..EnvSetup::default()
        })
        .await
        .expect("start");
        repl
    }

    #[tokio::test]
    async fn file_helpers_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("note.txt");
        let path = path.to_str().unwrap();
        let mut repl = started().await;

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
    async fn bash_runs_commands() {
        let mut repl = started().await;
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
        let mut repl = started().await;

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
    async fn cwd_setting_controls_working_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("marker.txt"), "here\n").unwrap();
        let mut repl = PythonRepl::new(Arc::new(NoSubcalls));
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
    async fn prelude_internals_hidden_from_vars() {
        let mut repl = started().await;
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
