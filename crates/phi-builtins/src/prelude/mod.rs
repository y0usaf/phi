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
    r##"Pre-defined helper functions (already available in the REPL — call them directly, do not redefine):
- bash(cmd: str, timeout: int = 120) -> str — run a shell command; returns combined stdout/stderr plus a note on non-zero exit; large output (>50KB) spills to a tmp file with a [full output: path] annotation
- read_file(path: str, offset: int = 0, limit: int = 2000, raw: bool = False, fresh: bool = False) -> str — read a file as `[path#TAG]` + `LINE:TEXT` rows (hashline format, default); pass raw=True for plain text without line numbers or tags; repeated reads of an unchanged file return a one-line stub instead of re-emitting content — pass fresh=True to force a full re-read
- write_file(path: str, content: str) -> str — write a file, creating parent directories; refuses to overwrite an existing file this session hasn't read (read_file it first)
- edit_file(path: str | None = None, old: str | None = None, new: str | None = None, replace_all: bool = False, patch: str | None = None) -> str — edit a file, two modes; both refuse to touch a file this session hasn't read or that changed on disk since the last read (re-read with fresh=True, then retry).
  Preferred — hashline patch: edit_file(patch=r"""...""") anchors edits to the line numbers and #TAG of the latest read_file/grep view, so you never re-quote or escape existing file content inside a Python string. Format: first line is the `[path#TAG]` header copied from the view, then ops — `SWAP N.=M:` replaces lines N–M with the `+TEXT` rows that follow; `DEL N.=M` deletes lines N–M (no colon, no body rows); `INS.POST N:` / `INS.PRE N:` insert the `+TEXT` rows after/before line N; `INS.HEAD:` / `INS.TAIL:` insert at the start/end of the file. `+TEXT` payload rows are literal file lines (everything after the `+`, indentation included). All line numbers refer to the file as last read — never shift them to account for earlier ops in the same patch; ranges must not overlap. One patch may carry several ops and several `[path#TAG]` sections. On success you get the fresh view (new #TAG) back; a stale #TAG auto-recovers when the anchored lines still match.
  Fallback — exact string replacement: edit_file(path, old, new, replace_all=False) for short, unambiguous substitutions where quoting `old` inside a Python string is painless.
- grep(pattern: str, path: str = ".", glob: str | None = None, max_results: int = 200, raw: bool = False) -> str — regex search across files (skips .git, node_modules, target, etc.); returns `[path#TAG]` + `LINE:TEXT` rows by default, pass raw=True for plain `path:line: text` output
- find(pattern: str, path: str = ".", max_results: int = 200) -> str — find files by glob on filename
- ls(path: str = ".") -> str — list a directory (dirs end with /)

Most helpers return strings (print them to inspect); `print(...)` output is truncated to 2000 lines / 50KB.
These run on the user's real machine in the current working directory — edits and commands have real effects.
For long-running work (builds, test suites, servers) prefer a background job, e.g. `jid = bg("out = bash('cargo build', timeout=3600)")`, then poll `job_status(jid)` / `job_output(jid)` on later turns instead of blocking this one.

Compose helpers in Python instead of spending a turn per call. One-turn idioms:

Search, filter in Python, read only what matters:
    hits = grep(r"fn apply_action", "crates", raw=True).splitlines()
    for h in hits[:10]:
        path, line = h.split(":", 2)[:2]
        print(read_file(path, offset=max(0, int(line) - 5), limit=20))

Edit with a hashline patch — header and line numbers copied from the view just printed; payload rows are literal, nothing escaped:
    print(edit_file(patch=r"""[crates/phi-app/src/cli.rs#4F2A]
SWAP 12.=13:
+    let verbose = args.verbose;
+    init_logging(verbose);
INS.POST 20:
+    log::debug!("cli ready");
DEL 31.=32
"""))

Apply one mechanical change across many files, then verify, in the same turn:
    for f in find("*.rs", "crates/phi-ext/src").splitlines():
        read_file(f)  # satisfy read-before-edit
        print(f, edit_file(f, "old_name(", "new_name(", replace_all=True).splitlines()[0])
    print(bash("cargo check -p phi-ext 2>&1 | tail -5", timeout=300))"##
        .to_owned()
}

#[cfg(test)]
mod tests;
