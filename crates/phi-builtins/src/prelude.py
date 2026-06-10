import subprocess as _subprocess
import os as _os
import re as _re
import fnmatch as _fnmatch

_PRELUDE_MAX_LINES = 2000
_PRELUDE_MAX_BYTES = 50_000
_PRELUDE_SKIP_DIRS = (".git", "node_modules", "target", "__pycache__", ".venv", "dist", "build")


def _prelude_truncate(text):
    lines = text.split("\n")
    if len(lines) > _PRELUDE_MAX_LINES:
        text = "\n".join(lines[:_PRELUDE_MAX_LINES])
        text += "\n[truncated: %d more lines]" % (len(lines) - _PRELUDE_MAX_LINES)
    if len(text) > _PRELUDE_MAX_BYTES:
        text = text[:_PRELUDE_MAX_BYTES] + "\n[truncated at %d bytes]" % _PRELUDE_MAX_BYTES
    return text


def bash(cmd, timeout=120):
    try:
        proc = _subprocess.run(
            cmd, shell=True, capture_output=True, text=True, timeout=timeout
        )
    except _subprocess.TimeoutExpired:
        return "Error: command timed out after %ss" % timeout
    out = proc.stdout
    if proc.stderr:
        out += ("\n" if out else "") + proc.stderr
    if proc.returncode != 0:
        out += ("\n" if out else "") + "[exit code %d]" % proc.returncode
    return _prelude_truncate(out) if out else "(no output)"


def read_file(path, offset=0, limit=2000):
    with open(path, errors="replace") as f:
        lines = f.read().split("\n")
    chunk = lines[offset : offset + limit]
    text = "\n".join(chunk)
    if offset + limit < len(lines):
        text += "\n[truncated: file has %d lines, showing %d-%d]" % (
            len(lines),
            offset,
            offset + len(chunk),
        )
    return _prelude_truncate(text)


def write_file(path, content):
    parent = _os.path.dirname(_os.path.abspath(path))
    if parent:
        _os.makedirs(parent, exist_ok=True)
    with open(path, "w") as f:
        f.write(content)
    return "Wrote %d bytes to %s" % (len(content.encode()), path)


def edit_file(path, old, new, replace_all=False):
    with open(path, errors="replace") as f:
        content = f.read()
    count = content.count(old)
    if count == 0:
        return "Error: string not found in %s" % path
    if count > 1 and not replace_all:
        return (
            "Error: string occurs %d times in %s; pass replace_all=True or a more specific string"
            % (count, path)
        )
    content = content.replace(old, new)
    with open(path, "w") as f:
        f.write(content)
    return "Replaced %d occurrence(s) in %s" % (count if replace_all else 1, path)


def grep(pattern, path=".", glob=None, max_results=200):
    rx = _re.compile(pattern)
    results = []
    for root, dirs, files in _os.walk(path):
        dirs[:] = [d for d in dirs if d not in _PRELUDE_SKIP_DIRS]
        for name in sorted(files):
            if glob and not _fnmatch.fnmatch(name, glob):
                continue
            fp = _os.path.join(root, name)
            try:
                with open(fp, errors="ignore") as f:
                    for i, line in enumerate(f, 1):
                        if rx.search(line):
                            results.append("%s:%d: %s" % (fp, i, line.rstrip()[:400]))
                            if len(results) >= max_results:
                                results.append("[truncated at %d matches]" % max_results)
                                return "\n".join(results)
            except OSError:
                continue
    return "\n".join(results) if results else "(no matches)"


def find(pattern, path=".", max_results=200):
    results = []
    for root, dirs, files in _os.walk(path):
        dirs[:] = [d for d in dirs if d not in _PRELUDE_SKIP_DIRS]
        for name in sorted(files):
            if _fnmatch.fnmatch(name, pattern):
                results.append(_os.path.join(root, name))
                if len(results) >= max_results:
                    results.append("[truncated at %d matches]" % max_results)
                    return "\n".join(results)
    return "\n".join(results) if results else "(no matches)"


def ls(path="."):
    entries = sorted(_os.listdir(path))
    out = []
    for name in entries:
        full = _os.path.join(path, name)
        out.append(name + "/" if _os.path.isdir(full) else name)
    return _prelude_truncate("\n".join(out)) if out else "(empty)"
