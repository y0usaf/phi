def read_file(path, offset=0, limit=2000, raw=False):
    with open(path, errors="replace") as f:
        raw_text = f.read()
    _bom, full_text = _hashline_strip_bom(raw_text)
    full_text = _hashline_normalize_to_lf(full_text)
    lines = _hashline_lines(full_text)
    if raw:
        chunk = lines[offset : offset + limit]
        text = "\n".join(chunk)
        if offset + limit < len(lines):
            text += "\n[truncated: file has %d lines, showing %d-%d]" % (
                len(lines),
                offset,
                offset + len(chunk),
            )
        return _prelude_truncate(text)
    start_line = int(offset) + 1
    seen_lines = range(start_line, min(len(lines), start_line + limit - 1) + 1)
    return _hashline_format_view(path, full_text, start_line, limit, seen_lines=seen_lines)


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
def edit_file(path=None, old=None, new=None, replace_all=False, patch=None):
    if patch is not None:
        try:
            sections = _hashline_parse_patch(patch)
            planned = []
            seen_paths = set()
            for section in sections:
                canonical = _hashline_canonical_path(section["path"])
                if canonical in seen_paths:
                    raise ValueError(
                        "multiple hashline sections resolve to the same file (%s); merge their ops under one header"
                        % section["path"]
                    )
                seen_paths.add(canonical)
                planned.append(_hashline_prepare_section(section))
            rendered = []
            for item in planned:
                after_normalized = item["after"]
                if item.get("bom"):
                    after_normalized = item["bom"] + after_normalized
                if item.get("line_ending") and item["line_ending"] != "\n":
                    after_normalized = _hashline_restore_line_endings(after_normalized, item["line_ending"])
                _path = item["path"]
                _dir = _os.path.dirname(_os.path.abspath(_path))
                if _dir:
                    _os.makedirs(_dir, exist_ok=True)
                _tmp = _path + ".phi-tmp"
                with open(_tmp, "w") as f:
                    f.write(after_normalized)
                _os.replace(_tmp, _path)
                lines = _hashline_lines(item["after"])
                display_limit = min(len(lines), 2000)
                seen = range(1, display_limit + 1)
                tag = _hashline_record_snapshot(item["path"], item["after"], seen)
                rendered.append(
                    _hashline_format_view(
                        item["path"], item["after"], 1, 2000, tag=tag, seen_lines=seen
                    )
                )
                for warning in item["warnings"]:
                    rendered.append("[warning] %s" % warning)
            return "\n\n".join(rendered)
        except Exception as e:
            return "Error: %s" % e
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
