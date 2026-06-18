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


@_tool
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



@_tool
def ls(path="."):
    entries = sorted(_os.listdir(path))
    out = []
    for name in entries:
        full = _os.path.join(path, name)
        out.append(name + "/" if _os.path.isdir(full) else name)
    return _prelude_truncate("\n".join(out)) if out else "(empty)"


_HASHLINE_HISTORY_LIMIT = 8
_hashline_snapshots = {}

