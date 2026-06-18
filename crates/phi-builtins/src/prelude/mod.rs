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
    include_str!("hashline.py"),
    include_str!("fusion.py"),
);

pub fn coding_tools_section() -> String {
    "Pre-defined helper functions (already available in the REPL — call them directly, do not redefine):
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
mod tests;
