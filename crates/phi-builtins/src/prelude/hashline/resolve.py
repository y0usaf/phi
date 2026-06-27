# ─── Block resolution (existing, unchanged) ────────────────────────────────

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


# ─── Stale recovery (SequenceMatcher, enhanced messages) ────────────────────

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
        "hunk touches unseen line(s) in %s: %s. Re-read those exact lines with read_file first."
        % (path, sample)
    )


# ─── Delimiter balance repair ──────────────────────────────────────────────

_STRUCTURAL_CLOSER_RE = _re.compile(r"^\s*[)\]}]+[;,]?\s*$")


def _hashline_compute_delimiter_delta(lines):
    """Net ()[]{] delta across lines, skipping // line comments, strings."""
    paren = bracket = brace = 0
    in_block_comment = False
    in_string = None  # ' or " or ` (only ` can span lines)
    for line in lines:
        j = 0
        while j < len(line):
            ch = line[j]
            if in_block_comment:
                if ch == "*" and j + 1 < len(line) and line[j + 1] == "/":
                    j += 2
                    in_block_comment = False
                    continue
                j += 1
                continue
            if in_string:
                if ch == "\\":
                    j += 2
                    continue
                if ch == in_string:
                    in_string = None
                j += 1
                continue
            if ch == "/" and j + 1 < len(line):
                if line[j + 1] == "/":
                    break  # line comment
                if line[j + 1] == "*":
                    in_block_comment = True
                    j += 2
                    continue
            if ch == '"' or ch == "'":
                in_string = ch
                j += 1
                continue
            if ch == "`":
                in_string = "`"
                j += 1
                continue
            if ch == "(":
                paren += 1
            elif ch == ")":
                paren -= 1
            elif ch == "[":
                bracket += 1
            elif ch == "]":
                bracket -= 1
            elif ch == "{":
                brace += 1
            elif ch == "}":
                brace -= 1
            j += 1
        # " and ' reset at EOL; ` continues
        if in_string in ('"', "'"):
            in_string = None
    return {"paren": paren, "bracket": bracket, "brace": brace}


def _hashline_repair_boundaries(lines, op):
    """Repair replacement boundary errors: duplicated closers/openers.

    Returns (repaired_payload, warnings).
    """
    payload = list(op["payload"])
    start = op["start"]
    end = op["end"]
    warnings = []

    if not payload:
        return payload, warnings

    # Original lines being replaced
    original = lines[start - 1 : end]

    # Lines just outside the range that survive
    before_line = lines[start - 2] if start > 1 else None
    after_line = lines[end] if end < len(lines) else None

    # Boundary echo: payload starts with the line before the range
    if before_line is not None and payload and payload[0] == before_line:
        if after_line is not None and payload[-1] == after_line:
            # Double boundary echo — drop both
            payload = payload[1:-1]
            warnings.append(
                "Replacement-boundary repair: dropped duplicated boundary lines at both edges "
                "(delimiter-balance)."
            )
        else:
            # Single leading echo — check delimiter balance
            delta_with = _hashline_compute_delimiter_delta(payload)
            delta_without = _hashline_compute_delimiter_delta(payload[1:])
            orig_delta = _hashline_compute_delimiter_delta(original)
            # If dropping the echo improves balance toward original, do it
            if (abs(delta_without["brace"]) < abs(delta_with["brace"])
                or abs(delta_without["paren"]) < abs(delta_with["paren"])
                or abs(delta_without["bracket"]) < abs(delta_with["bracket"])):
                if _STRUCTURAL_CLOSER_RE.match(payload[0]) or _STRUCTURAL_CLOSER_RE.match(before_line):
                    payload = payload[1:]
                    warnings.append(
                        "Replacement-boundary repair: dropped a duplicated structural closer "
                        "at the leading edge (delimiter-balance)."
                    )

    # Trailing echo: payload ends with the line after the range
    if after_line is not None and payload and payload[-1] == after_line:
        delta_with = _hashline_compute_delimiter_delta(payload)
        delta_without = _hashline_compute_delimiter_delta(payload[:-1])
        if (abs(delta_without["brace"]) < abs(delta_with["brace"])
            or abs(delta_without["paren"]) < abs(delta_with["paren"])
            or abs(delta_without["bracket"]) < abs(delta_with["bracket"])):
            if _STRUCTURAL_CLOSER_RE.match(payload[-1]) or _STRUCTURAL_CLOSER_RE.match(after_line):
                payload = payload[:-1]
                warnings.append(
                    "Replacement-boundary repair: dropped a duplicated structural closer "
                    "at the trailing edge (delimiter-balance)."
                )

    # Leading echo: payload starts with the line before the range (recheck after trailing trim)
    if before_line is not None and payload and payload[0] == before_line:
        delta_with = _hashline_compute_delimiter_delta(payload)
        delta_without = _hashline_compute_delimiter_delta(payload[1:])
        if (abs(delta_without["brace"]) < abs(delta_with["brace"])
            or abs(delta_without["paren"]) < abs(delta_with["paren"])
            or abs(delta_without["bracket"]) < abs(delta_with["bracket"])):
            if _STRUCTURAL_CLOSER_RE.match(payload[0]) or _STRUCTURAL_CLOSER_RE.match(before_line):
                payload = payload[1:]
                warnings.append(
                    "Replacement-boundary repair: dropped a duplicated structural closer "
                    "at the leading edge (delimiter-balance)."
                )

    return payload, warnings


# ─── Op lowering ───────────────────────────────────────────────────────────

def _hashline_lower_ops(section, snapshot, target_lines, line_map=None):
    concrete = []
    repair_warnings = []
    for op in section["ops"]:
        kind = op["kind"]
        if kind == "swap":
            _hashline_require_seen(snapshot, range(op["start"], op["end"] + 1), section["path"])
            start, end = op["start"], op["end"]
            if line_map is not None:
                start, end = _hashline_map_range(line_map, start, end)
            payload, pwarns = _hashline_repair_boundaries(target_lines, op)
            repair_warnings.extend(pwarns)
            # Noop detection: if payload matches the original lines exactly
            original = target_lines[start - 1 : end]
            if original == payload:
                repair_warnings.append("Replacement produced no change (noop).")
            concrete.append({"kind": "replace", "start": start, "end": end, "payload": payload})
        elif kind == "delete":
            _hashline_require_seen(snapshot, range(op["start"], op["end"] + 1), section["path"])
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
            payload, pwarns = _hashline_repair_boundaries(target_lines, {"start": start, "end": end, "payload": op["payload"]})
            repair_warnings.extend(pwarns)
            concrete.append({"kind": "replace", "start": start, "end": end, "payload": list(payload)})
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
                {"kind": "insert", "where": op["where"], "anchor": anchor, "payload": list(op["payload"])}
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
    # Attach repair warnings to the section
    section.setdefault("warnings", []).extend(repair_warnings)
    return concrete


