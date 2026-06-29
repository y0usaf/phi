//! Coding-agent prelude: Python helpers injected into the REPL via
//! `RlmConfig::setup_code`, plus the system-prompt docs for them.
//!
//! The Python prelude is split into one file per tool/concern and
//! reassembled here via `concat!`.

pub const CODING_PRELUDE: &str = concat!(
    include_str!("_core.py"),
    include_str!("bash.py"),
    include_str!("file_ops.py"),
    include_str!("search.py"),
    include_str!("hashline/normalize.py"),
    include_str!("hashline/hashing.py"),
    include_str!("hashline/snapshot.py"),
    include_str!("hashline/view.py"),
    include_str!("hashline/parse.py"),
    include_str!("hashline/resolve.py"),
    include_str!("hashline/apply.py"),
);

pub fn coding_tools_section() -> String {
    "Pre-defined helper functions (already available in the REPL — call them directly, do not redefine):
- shell(cmd: str, timeout: int = 120) -> str — run a shell command; returns combined stdout/stderr plus a note on non-zero exit
- read_file(path: str, offset: int = 0, limit: int = 2000, raw: bool = False) -> str — read a file as `[path#TAG]` + `LINE:TEXT` rows (hashline format, default); pass raw=True for plain text without line numbers or tags
- write_file(path: str, content: str) -> str — write a file, creating parent directories
- edit_file(path: str | None = None, old: str | None = None, new: str | None = None, replace_all: bool = False, patch: str | None = None) -> str — exact string replacement (old/new) OR hashline patch application (patch); pass patch=\"...\" to apply `[path#TAG]` + `SWAP`/`DEL`/`INS.*` ops with snapshot-tag checks and stale-tag recovery
- grep(pattern: str, path: str = \".\", glob: str | None = None, max_results: int = 200, raw: bool = False) -> str — regex search across files (skips .git, node_modules, target, etc.); returns `[path#TAG]` + `LINE:TEXT` rows by default, pass raw=True for plain `path:line: text` output
- find(pattern: str, path: str = \".\", max_results: int = 200) -> str — find files by glob on filename
- ls(path: str = \".\") -> str — list a directory (dirs end with /)

Most helpers return strings (print them to inspect); `print(...)` output is truncated to 2000 lines / 50KB.
These run on the user's real machine in the current working directory — edits and commands have real effects.
For long-running work (builds, test suites, servers) prefer a background job, e.g. `jid = bg(\"out = shell('cargo build', timeout=3600)\")`, then poll `job_status(jid)` / `job_output(jid)` on later turns instead of blocking this one."
        .to_owned()
}

#[cfg(test)]
mod tests;
