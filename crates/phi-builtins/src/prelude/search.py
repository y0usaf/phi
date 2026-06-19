def grep(pattern, path=".", glob=None, max_results=200, raw=False):
    rx = _re.compile(pattern)
    if raw:
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
    grouped = []
    total = 0
    for root, dirs, files in _os.walk(path):
        dirs[:] = [d for d in dirs if d not in _PRELUDE_SKIP_DIRS]
        for name in sorted(files):
            if glob and not _fnmatch.fnmatch(name, glob):
                continue
            fp = _os.path.join(root, name)
            matches = []
            try:
                with open(fp, errors="replace") as f:
                    full_text = f.read()
            except OSError:
                continue
            for i, line in enumerate(_hashline_lines(full_text), 1):
                if rx.search(line):
                    matches.append(i)
                    total += 1
                    if total >= max_results:
                        break
            if matches:
                tag = _hashline_record_snapshot(fp, full_text, matches)
                grouped.append((fp, tag, full_text, matches))
            if total >= max_results:
                break
        if total >= max_results:
            break
    if not grouped:
        return "(no matches)"
    out = []
    for fp, tag, full_text, matches in grouped:
        out.append("[%s#%s]" % (fp, tag))
        lines = _hashline_lines(full_text)
        for line_no in matches:
            out.append("%d:%s" % (line_no, lines[line_no - 1]))
    if total >= max_results:
        out.append("[truncated at %d matches]" % max_results)
    return _prelude_truncate("\n".join(out))


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

