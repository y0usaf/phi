import subprocess as _subprocess
import os as _os
import re as _re
import json as _json
import zlib as _zlib
import fnmatch as _fnmatch
import inspect as _inspect
import time as _time
import difflib as _difflib
from concurrent.futures import ThreadPoolExecutor as _ThreadPoolExecutor

_PRELUDE_MAX_LINES = 2000
_PRELUDE_MAX_BYTES = 50_000
_PRELUDE_SKIP_DIRS = (".git", "node_modules", "target", "__pycache__", ".venv", "dist", "build")

_tool_calls = [0]


def _tool(fn):
    """Emit driver tool_event frames around each call so the host TUI can
    render the call nested inside its ```repl``` block, live."""

    def _wrapped(*args, **kwargs):
        _tool_calls[0] += 1
        cid = _tool_calls[0]
        try:
            params = dict(_inspect.signature(fn).bind(*args, **kwargs).arguments)
        except TypeError:
            params = {}
        __phi_tool_event__("start", fn.__name__, cid, {"args": params})
        t0 = _time.time()
        try:
            result = fn(*args, **kwargs)
        except BaseException as e:
            __phi_tool_event__(
                "end",
                fn.__name__,
                cid,
                {"error": "%s: %s" % (type(e).__name__, e), "duration": _time.time() - t0},
            )
            raise
        error = result if isinstance(result, str) and result.startswith("Error:") else None
        __phi_tool_event__(
            "end",
            fn.__name__,
            cid,
            {
                "result": result if isinstance(result, str) else repr(result),
                "error": error,
                "duration": _time.time() - t0,
            },
        )
        return result

    _wrapped.__name__ = fn.__name__
    _wrapped.__doc__ = fn.__doc__
    return _wrapped


def _prelude_truncate(text):
    lines = text.split("\n")
    if len(lines) > _PRELUDE_MAX_LINES:
        text = "\n".join(lines[:_PRELUDE_MAX_LINES])
        text += "\n[truncated: %d more lines]" % (len(lines) - _PRELUDE_MAX_LINES)
    if len(text) > _PRELUDE_MAX_BYTES:
        text = text[:_PRELUDE_MAX_BYTES] + "\n[truncated at %d bytes]" % _PRELUDE_MAX_BYTES
    return text


def _format_unified_diff(path, before, after):
    before_lines = before.splitlines(True)
    after_lines = after.splitlines(True)
    diff = "".join(
        _difflib.unified_diff(
            before_lines,
            after_lines,
            fromfile=path,
            tofile=path,
            lineterm="",
        )
    )
    return _prelude_truncate(diff) if diff else "(no diff)"


@_tool
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


@_tool
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


def _hashline_canonical_path(path):
    return _os.path.abspath(path)


def _hashline_lines(text):
    return text.split("\n")


def _hashline_all_lines(text):
    return range(1, len(_hashline_lines(text)) + 1)


def _hashline_normalize_hash_text(text):
    return _re.sub(r"[ \t\r]+(?=\n|$)", "", text)


def _hashline_compute_tag(text):
    data = _hashline_normalize_hash_text(text).encode("utf-8", "surrogatepass")
    return "%04X" % (_zlib.crc32(data) & 0xFFFF)


def _hashline_merge_seen_lines(snapshot, seen_lines):
    if seen_lines is None:
        return
    if snapshot.get("seen_lines") is None:
        snapshot["seen_lines"] = set()
    for line in seen_lines:
        snapshot["seen_lines"].add(int(line))


def _hashline_record_snapshot(path, full_text, seen_lines=None):
    canonical = _hashline_canonical_path(path)
    display_path = str(path)
    tag = _hashline_compute_tag(full_text)
    history = _hashline_snapshots.setdefault(canonical, [])
    for i, snapshot in enumerate(history):
        if snapshot["tag"] == tag:
            snapshot["text"] = full_text
            snapshot["display_path"] = display_path
            _hashline_merge_seen_lines(snapshot, seen_lines)
            if i != 0:
                history.insert(0, history.pop(i))
            return tag
    snapshot = {
        "canonical_path": canonical,
        "display_path": display_path,
        "tag": tag,
        "text": full_text,
        "seen_lines": None,
    }
    _hashline_merge_seen_lines(snapshot, seen_lines)
    history.insert(0, snapshot)
    del history[_HASHLINE_HISTORY_LIMIT:]
    return tag


def _hashline_find_snapshot(path, tag):
    history = _hashline_snapshots.get(_hashline_canonical_path(path), [])
    for snapshot in history:
        if snapshot["tag"] == str(tag).upper():
            return snapshot
    return None


def _hashline_format_view(path, full_text, start_line=1, limit=2000, tag=None, seen_lines=None):
    start_line = int(start_line)
    limit = int(limit)
    if start_line < 1:
        raise ValueError("start_line must be >= 1")
    if limit < 1:
        raise ValueError("limit must be >= 1")
    lines = _hashline_lines(full_text)
    start_idx = start_line - 1
    chunk = lines[start_idx : start_idx + limit]
    if tag is None:
        tag = _hashline_record_snapshot(path, full_text, seen_lines)
    elif seen_lines is not None:
        _hashline_record_snapshot(path, full_text, seen_lines)
    out = ["[%s#%s]" % (path, tag)]
    for i, line in enumerate(chunk, start_line):
        out.append("%d:%s" % (i, line))
    if start_idx + limit < len(lines):
        out.append(
            "[truncated: file has %d lines, showing %d-%d]"
            % (len(lines), start_line, start_line + len(chunk) - 1)
        )
    return _prelude_truncate("\n".join(out))


def _hashline_header_re():
    return _re.compile(r"^\[([^#\r\n]+)#([0-9A-Fa-f]{4})\]$")


def _hashline_parse_patch(patch):
    lines = str(patch).replace("\r\n", "\n").replace("\r", "\n").split("\n")
    while lines and (not lines[0].strip() or lines[0].strip() == "*** Begin Patch"):
        lines.pop(0)
    sections = []
    i = 0
    header_rx = _hashline_header_re()
    while i < len(lines):
        line = lines[i].strip()
        if not line:
            i += 1
            continue
        if line == "*** End Patch":
            break
        m = header_rx.match(line)
        if not m:
            raise ValueError(
                'input must begin with "[PATH#TAG]" on the first non-blank patch line; got %r'
                % lines[i]
            )
        section = {"path": m.group(1), "tag": m.group(2).upper(), "ops": []}
        i += 1
        while i < len(lines):
            raw = lines[i]
            stripped = raw.strip()
            if stripped == "*** End Patch":
                i = len(lines)
                break
            if not stripped:
                i += 1
                continue
            if header_rx.match(stripped):
                break

            def take_payload():
                nonlocal i
                payload = []
                i += 1
                while i < len(lines) and lines[i].startswith("+"):
                    payload.append(lines[i][1:])
                    i += 1
                return payload

            m = _re.match(r"^SWAP (\d+)\.=(\d+):$", stripped)
            if m:
                payload = take_payload()
                if not payload:
                    raise ValueError("SWAP %s.= %s needs at least one +TEXT payload line" % (m.group(1), m.group(2)))
                section["ops"].append(
                    {
                        "kind": "swap",
                        "start": int(m.group(1)),
                        "end": int(m.group(2)),
                        "payload": payload,
                    }
                )
                continue
            m = _re.match(r"^SWAP\.BLK (\d+):$", stripped)
            if m:
                payload = take_payload()
                if not payload:
                    raise ValueError("SWAP.BLK %s needs at least one +TEXT payload line" % m.group(1))
                section["ops"].append(
                    {"kind": "swap_block", "anchor": int(m.group(1)), "payload": payload}
                )
                continue
            m = _re.match(r"^DEL (\d+)(?:\.=(\d+))?$", stripped)
            if m:
                section["ops"].append(
                    {
                        "kind": "delete",
                        "start": int(m.group(1)),
                        "end": int(m.group(2) or m.group(1)),
                    }
                )
                i += 1
                continue
            m = _re.match(r"^DEL\.BLK (\d+)$", stripped)
            if m:
                section["ops"].append({"kind": "delete_block", "anchor": int(m.group(1))})
                i += 1
                continue
            m = _re.match(r"^INS\.(PRE|POST) (\d+):$", stripped)
            if m:
                payload = take_payload()
                if not payload:
                    raise ValueError("INS.%s %s needs at least one +TEXT payload line" % (m.group(1), m.group(2)))
                section["ops"].append(
                    {
                        "kind": "insert",
                        "where": m.group(1),
                        "anchor": int(m.group(2)),
                        "payload": payload,
                    }
                )
                continue
            m = _re.match(r"^INS\.BLK\.POST (\d+):$", stripped)
            if m:
                payload = take_payload()
                if not payload:
                    raise ValueError("INS.BLK.POST %s needs at least one +TEXT payload line" % m.group(1))
                section["ops"].append(
                    {"kind": "insert_block_post", "anchor": int(m.group(1)), "payload": payload}
                )
                continue
            m = _re.match(r"^INS\.(HEAD|TAIL):$", stripped)
            if m:
                payload = take_payload()
                if not payload:
                    raise ValueError("INS.%s needs at least one +TEXT payload line" % m.group(1))
                section["ops"].append(
                    {"kind": "insert_edge", "where": m.group(1), "payload": payload}
                )
                continue
            raise ValueError("unrecognized hashline op: %r" % raw)
        if not section["ops"]:
            raise ValueError("section for %s contains no edit ops" % section["path"])
        sections.append(section)
    if not sections:
        raise ValueError("patch contained no hashline sections")
    return sections


def _hashline_indent_width(line):
    line = str(line)
    return len(line) - len(line.lstrip(" \t"))


def _hashline_find_matching_brace(lines, start_line):
    balance = 0
    opened = False
    for idx in range(start_line - 1, len(lines)):
        line = lines[idx]
        for ch in line:
            if ch == "{":
                balance += 1
                opened = True
            elif ch == "}":
                if opened:
                    balance -= 1
        if opened and balance <= 0 and idx + 1 > start_line:
            return idx + 1
    return None


def _hashline_resolve_indent_block(lines, start_line):
    anchor = lines[start_line - 1]
    anchor_indent = _hashline_indent_width(anchor)
    body_start = None
    for idx in range(start_line, len(lines)):
        stripped = lines[idx].strip()
        if not stripped:
            continue
        if _hashline_indent_width(lines[idx]) > anchor_indent:
            body_start = idx + 1
        break
    if body_start is None:
        return None
    end = body_start
    last_content = body_start
    for idx in range(body_start - 1, len(lines)):
        stripped = lines[idx].strip()
        if stripped:
            indent = _hashline_indent_width(lines[idx])
            if idx + 1 != body_start and indent <= anchor_indent:
                break
            last_content = idx + 1
        end = idx + 1
    return last_content


def _hashline_resolve_block(lines, start_line):
    start_line = int(start_line)
    if start_line < 1 or start_line > len(lines):
        raise ValueError("line %d does not exist (file has %d lines)" % (start_line, len(lines)))
    stripped = lines[start_line - 1].strip()
    if stripped.startswith("@") and start_line < len(lines):
        nested_start, nested_end = _hashline_resolve_block(lines, start_line + 1)
        return start_line, nested_end
    brace_end = _hashline_find_matching_brace(lines, start_line)
    if brace_end is not None and brace_end > start_line:
        return start_line, brace_end
    indent_end = _hashline_resolve_indent_block(lines, start_line)
    if indent_end is not None and indent_end > start_line:
        return start_line, indent_end
    raise ValueError(
        "could not resolve a multi-line block starting at line %d; use plain SWAP/DEL/INS.POST instead"
        % start_line
    )


def _hashline_build_equal_line_map(base_lines, live_lines):
    matcher = _difflib.SequenceMatcher(a=base_lines, b=live_lines, autojunk=False)
    mapping = {}
    for tag, i1, i2, j1, j2 in matcher.get_opcodes():
        if tag == "equal":
            for delta in range(i2 - i1):
                mapping[i1 + delta + 1] = j1 + delta + 1
    return mapping


def _hashline_map_range(mapping, start, end):
    mapped = [mapping.get(line) for line in range(start, end + 1)]
    if any(value is None for value in mapped):
        raise ValueError(
            "stale anchor could not be safely recovered for lines %d-%d; re-read before editing"
            % (start, end)
        )
    if mapped != list(range(mapped[0], mapped[-1] + 1)):
        raise ValueError(
            "stale anchor for lines %d-%d became non-contiguous; re-read before editing"
            % (start, end)
        )
    return mapped[0], mapped[-1]


def _hashline_require_seen(snapshot, touched_lines, path):
    seen = snapshot.get("seen_lines")
    if seen is None:
        return
    missing = [line for line in touched_lines if int(line) not in seen]
    if not missing:
        return
    sample = ", ".join(str(line) for line in missing[:8])
    if len(missing) > 8:
        sample += ", ..."
    raise ValueError(
        "hunk touches unseen line(s) in %s: %s. Re-read those exact lines with read_hashline first."
        % (path, sample)
    )


def _hashline_lower_ops(section, snapshot, target_lines, line_map=None):
    concrete = []
    for op in section["ops"]:
        kind = op["kind"]
        if kind == "swap":
            _hashline_require_seen(
                snapshot, range(op["start"], op["end"] + 1), section["path"]
            )
            start, end = op["start"], op["end"]
            if line_map is not None:
                start, end = _hashline_map_range(line_map, start, end)
            concrete.append(
                {"kind": "replace", "start": start, "end": end, "payload": list(op["payload"])}
            )
        elif kind == "delete":
            _hashline_require_seen(
                snapshot, range(op["start"], op["end"] + 1), section["path"]
            )
            start, end = op["start"], op["end"]
            if line_map is not None:
                start, end = _hashline_map_range(line_map, start, end)
            concrete.append({"kind": "replace", "start": start, "end": end, "payload": []})
        elif kind == "swap_block":
            _hashline_require_seen(snapshot, [op["anchor"]], section["path"])
            anchor = op["anchor"] if line_map is None else line_map.get(op["anchor"])
            if anchor is None:
                raise ValueError(
                    "stale block anchor at line %d could not be recovered; re-read before editing"
                    % op["anchor"]
                )
            start, end = _hashline_resolve_block(target_lines, anchor)
            concrete.append(
                {"kind": "replace", "start": start, "end": end, "payload": list(op["payload"])}
            )
        elif kind == "delete_block":
            _hashline_require_seen(snapshot, [op["anchor"]], section["path"])
            anchor = op["anchor"] if line_map is None else line_map.get(op["anchor"])
            if anchor is None:
                raise ValueError(
                    "stale block anchor at line %d could not be recovered; re-read before editing"
                    % op["anchor"]
                )
            start, end = _hashline_resolve_block(target_lines, anchor)
            concrete.append({"kind": "replace", "start": start, "end": end, "payload": []})
        elif kind == "insert":
            _hashline_require_seen(snapshot, [op["anchor"]], section["path"])
            anchor = op["anchor"] if line_map is None else line_map.get(op["anchor"])
            if anchor is None:
                raise ValueError(
                    "stale insert anchor at line %d could not be recovered; re-read before editing"
                    % op["anchor"]
                )
            concrete.append(
                {
                    "kind": "insert",
                    "where": op["where"],
                    "anchor": anchor,
                    "payload": list(op["payload"]),
                }
            )
        elif kind == "insert_block_post":
            _hashline_require_seen(snapshot, [op["anchor"]], section["path"])
            anchor = op["anchor"] if line_map is None else line_map.get(op["anchor"])
            if anchor is None:
                raise ValueError(
                    "stale block anchor at line %d could not be recovered; re-read before editing"
                    % op["anchor"]
                )
            _, end = _hashline_resolve_block(target_lines, anchor)
            concrete.append(
                {"kind": "insert", "where": "POST", "anchor": end, "payload": list(op["payload"])}
            )
        elif kind == "insert_edge":
            if op["where"] == "HEAD" and target_lines:
                _hashline_require_seen(snapshot, [1], section["path"])
            if op["where"] == "TAIL" and target_lines:
                _hashline_require_seen(snapshot, [len(target_lines)], section["path"])
            concrete.append({"kind": "insert_edge", "where": op["where"], "payload": list(op["payload"])})
        else:
            raise ValueError("unknown lowered op kind: %s" % kind)
    return concrete


def _hashline_apply_concrete_ops(lines, ops):
    n = len(lines)
    ranges = []
    for op in ops:
        if op["kind"] == "replace":
            start = int(op["start"])
            end = int(op["end"])
            if start < 1 or end < start or end > n:
                raise ValueError("invalid range %d.=%d for file with %d lines" % (start, end, n))
            ranges.append(op)
    ranges.sort(key=lambda op: (op["start"], op["end"]))
    for prev, curr in zip(ranges, ranges[1:]):
        if curr["start"] <= prev["end"]:
            raise ValueError(
                "overlapping ranges %d.=%d and %d.=%d are not allowed"
                % (prev["start"], prev["end"], curr["start"], curr["end"])
            )

    covered = set()
    replacements = {}
    for op in ranges:
        replacements[op["start"]] = op
        for line in range(op["start"], op["end"] + 1):
            covered.add(line)

    inserts_before = {}
    inserts_after = {}
    head = []
    tail = []
    for op in ops:
        if op["kind"] == "insert":
            anchor = int(op["anchor"])
            if anchor < 1 or anchor > n:
                raise ValueError("line %d does not exist (file has %d lines)" % (anchor, n))
            if anchor in covered:
                raise ValueError(
                    "insert anchor at line %d lands inside another edited range; split the patch into tighter hunks"
                    % anchor
                )
            bucket = inserts_before if op["where"] == "PRE" else inserts_after
            bucket.setdefault(anchor, []).extend(op["payload"])
        elif op["kind"] == "insert_edge":
            if op["where"] == "HEAD":
                head.extend(op["payload"])
            else:
                tail.extend(op["payload"])

    out = []
    out.extend(head)
    line = 1
    while line <= n:
        out.extend(inserts_before.get(line, []))
        replacement = replacements.get(line)
        if replacement is not None:
            out.extend(replacement["payload"])
            end = replacement["end"]
            out.extend(inserts_after.get(end, []))
            line = end + 1
            continue
        out.append(lines[line - 1])
        out.extend(inserts_after.get(line, []))
        line += 1
    out.extend(tail)
    return "\n".join(out)


def _hashline_prepare_section(section):
    path = section["path"]
    with open(path, errors="replace") as f:
        live_text = f.read()
    live_lines = _hashline_lines(live_text)
    live_tag = _hashline_compute_tag(live_text)
    expected_tag = section["tag"]
    snapshot = _hashline_find_snapshot(path, expected_tag)
    warnings = []

    if live_tag == expected_tag:
        if snapshot is None:
            snapshot = {
                "tag": expected_tag,
                "text": live_text,
                "seen_lines": None,
            }
        target_lines = live_lines
        line_map = None
    else:
        if snapshot is None:
            raise ValueError(
                "stale or unknown hash tag %s for %s; re-read with read_hashline before editing"
                % (expected_tag, path)
            )
        base_lines = _hashline_lines(snapshot["text"])
        target_lines = live_lines
        line_map = _hashline_build_equal_line_map(base_lines, live_lines)
        warnings.append(
            "Recovered from stale hash tag using snapshot-based line remapping."
        )

    ops = _hashline_lower_ops(section, snapshot, target_lines, line_map)
    after_text = _hashline_apply_concrete_ops(target_lines, ops)
    return {
        "path": path,
        "canonical_path": _hashline_canonical_path(path),
        "before": live_text,
        "after": after_text,
        "warnings": warnings,
    }


@_tool
def read_hashline(path, start_line=1, limit=2000):
    with open(path, errors="replace") as f:
        full_text = f.read()
    lines = _hashline_lines(full_text)
    start_line = int(start_line)
    limit = int(limit)
    seen_lines = range(start_line, min(len(lines), start_line + limit - 1) + 1)
    return _hashline_format_view(path, full_text, start_line, limit, seen_lines=seen_lines)


@_tool
def grep_hashline(pattern, path=".", glob=None, max_results=200):
    rx = _re.compile(pattern)
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
def apply_hashline(patch):
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
            with open(item["path"], "w") as f:
                f.write(item["after"])
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


def _fusion_parse_json(text):
    text = str(text).strip()
    if not text:
        return None
    candidates = [text]
    fenced = _re.search(r"```(?:json)?\s*(\{.*?\})\s*```", text, _re.S)
    if fenced:
        candidates.append(fenced.group(1))
    start = text.find("{")
    end = text.rfind("}")
    if start != -1 and end != -1 and end > start:
        candidates.append(text[start : end + 1])
    for candidate in candidates:
        try:
            value = _json.loads(candidate)
        except Exception:
            continue
        if isinstance(value, dict):
            return value
    return None


def __phi_ext_tool__(name, args=None):
    """Call a Rust extension-registered tool via the subcall protocol."""
    result = _subcall({"kind": "ext_tool", "name": name, "args": args or {}})
    if result.get("error") is not None:
        return "Error: %s" % result["error"]
    return result.get("text", "")


def fusion(
    prompt,
    analysis_models=None,
    judge_model=None,
    perspectives=None,
    recursive=False,
    max_workers=8,
):
    """Run a Fusion-style multi-model / multi-perspective synthesis pass.

    Returns a dict with:
    - panel: list of {model, perspective, response}
    - synthesis: parsed JSON from the judge when possible, else {raw: ...}
    - final_answer: the judge's final answer field (or raw judge output)
    """

    query = rlm_query if recursive else llm_query
    prompt = str(prompt)

    if perspectives is None:
        perspectives = [
            "Direct answer: solve the user's request as straightforwardly as possible.",
            "Skeptical review: stress-test assumptions, edge cases, and possible mistakes.",
            "Implementation focus: emphasize concrete steps, code, commands, or exact evidence.",
            "Concise synthesis: focus on the minimum actionable conclusion and caveats.",
        ]
    elif isinstance(perspectives, str):
        perspectives = [perspectives]
    else:
        perspectives = list(perspectives)
    if not perspectives:
        raise ValueError("fusion(): perspectives must not be empty")

    if analysis_models is None:
        analysis_models = [None] * min(4, len(perspectives))
    elif isinstance(analysis_models, str):
        analysis_models = [analysis_models]
    else:
        analysis_models = list(analysis_models)
    if not analysis_models:
        analysis_models = [None] * min(4, len(perspectives))

    calls = []
    for i, model in enumerate(analysis_models):
        perspective = perspectives[i % len(perspectives)]
        analysis_prompt = (
            "You are one panelist in a Fusion-style deliberation.\n"
            "Perspective: %s\n\n"
            "Task:\n%s\n\n"
            "Respond with:\n"
            "1. Short answer\n"
            "2. Key evidence / reasoning\n"
            "3. Uncertainties / caveats\n"
            "Keep it concise but substantive."
        ) % (perspective, prompt)
        calls.append(
            {
                "model": model,
                "perspective": perspective,
                "prompt": analysis_prompt,
            }
        )

    workers = max(1, min(int(max_workers), len(calls)))
    if len(calls) == 1:
        responses = [query(calls[0]["prompt"], model=calls[0]["model"])]
    else:
        with _ThreadPoolExecutor(max_workers=workers) as pool:
            responses = list(
                pool.map(
                    lambda item: query(item["prompt"], model=item["model"]),
                    calls,
                )
            )

    panel = []
    for item, response in zip(calls, responses):
        panel.append(
            {
                "model": item["model"] or "default",
                "perspective": item["perspective"],
                "response": response,
            }
        )

    judge_prompt = (
        "You are the judge in a Fusion-style deliberation.\n"
        "Synthesize the panelists' answers into a single result.\n\n"
        "Original task:\n%s\n\n"
        "Return a JSON object with exactly these keys:\n"
        "- consensus: list[str]\n"
        "- contradictions: list[str]\n"
        "- partial_coverage: list[str]\n"
        "- unique_insights: list[str]\n"
        "- blind_spots: list[str]\n"
        "- final_answer: str\n\n"
        "Panel responses:\n"
    ) % prompt

    for i, item in enumerate(panel, 1):
        judge_prompt += (
            "\nPanelist %d\n"
            "Model: %s\n"
            "Perspective: %s\n"
            "Response:\n%s\n"
        ) % (i, item["model"], item["perspective"], item["response"])

    judge_text = query(judge_prompt, model=judge_model)
    synthesis = _fusion_parse_json(judge_text)
    final_answer = judge_text
    if synthesis is None:
        synthesis = {"raw": judge_text}
    else:
        final_answer = synthesis.get("final_answer", judge_text)

    return {
        "panel": panel,
        "judge_model": judge_model or "default",
        "synthesis": synthesis,
        "final_answer": final_answer,
    }
