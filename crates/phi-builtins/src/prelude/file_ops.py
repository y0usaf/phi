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


@_tool
def write_file(path, content):
    parent = _os.path.dirname(_os.path.abspath(path))
    if parent:
        _os.makedirs(parent, exist_ok=True)
    existed = _os.path.exists(path)
    before = ""
    if existed:
        with open(path, errors="replace") as f:
            before = f.read()
    with open(path, "w") as f:
        f.write(content)
    _hashline_record_snapshot(path, content, _hashline_all_lines(content))
    summary = "Wrote %d bytes to %s" % (len(content.encode()), path)
    if existed:
        return summary + "\n" + _format_unified_diff(path, before, content)
    return summary


@_tool
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
    updated = content.replace(old, new) if replace_all else content.replace(old, new, 1)
    with open(path, "w") as f:
        f.write(updated)
    _hashline_record_snapshot(path, updated, _hashline_all_lines(updated))
    return "Replaced %d occurrence(s) in %s\n%s" % (
        count if replace_all else 1,
        path,
        _format_unified_diff(path, content, updated),
    )


@_tool
