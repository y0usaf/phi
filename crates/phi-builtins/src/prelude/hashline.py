def _hashline_canonical_path(path):
    return _os.path.abspath(path)


# ─── Text normalization (BOM + line endings) ──────────────────────────────

def _hashline_strip_bom(text):
    """Strip a UTF-8 BOM if present; return (bom, text_without_bom)."""
    if text.startswith("\ufeff"):
        return "\ufeff", text[1:]
    return "", text


def _hashline_detect_line_ending(text):
    """Detect the first line-ending style in content. Returns r'\n' or r'\r\n'."""
    crlf = text.find("\r\n")
    lf = text.find("\n")
    if lf == -1:
        return "\n"
    if crlf == -1:
        return "\n"
    return "\r\n" if crlf < lf else "\n"


def _hashline_normalize_to_lf(text):
    """Normalize all line endings to LF."""
    return text.replace("\r\n", "\n").replace("\r", "\n")


def _hashline_restore_line_endings(text, ending):
    """Re-encode LF text with the requested line ending."""
    if ending == "\r\n":
        return text.replace("\n", "\r\n")
    return text


# ─── Line helpers ──────────────────────────────────────────────────────────

def _hashline_lines(text):
    return text.split("\n")


def _hashline_all_lines(text):
    return range(1, len(_hashline_lines(text)) + 1)


# ─── Hashing ───────────────────────────────────────────────────────────────

def _hashline_normalize_hash_text(text):
    return _re.sub(r"[ \t\r]+(?=\n|$)", "", text)


def _hashline_compute_tag(text):
    data = _hashline_normalize_hash_text(text).encode("utf-8", "surrogatepass")
    return "%04X" % (_zlib.crc32(data) & 0xFFFF)


# ─── Snapshot store ────────────────────────────────────────────────────────

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


# ─── Format view ───────────────────────────────────────────────────────────

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


# ─── Mismatch diagnostics ──────────────────────────────────────────────────

_MISMATCH_CONTEXT = 2

def _hashline_format_anchor_context(anchor_lines, file_lines):
    """Format numbered LINE:TEXT rows around anchors with *-marking and ... gaps."""
    display = set()
    for line in anchor_lines:
        if 1 <= line <= len(file_lines):
            lo = max(1, line - _MISMATCH_CONTEXT)
            hi = min(len(file_lines), line + _MISMATCH_CONTEXT)
            for n in range(lo, hi + 1):
                display.add(n)
    anchor_set = set(anchor_lines)
    rows = []
    prev = -1
    for n in sorted(display):
        if prev != -1 and n > prev + 1:
            rows.append("...")
        prev = n
        marker = "*" if n in anchor_set else " "
        rows.append("%s%d:%s" % (marker, n, file_lines[n - 1] if n <= len(file_lines) else ""))
    return rows


def _hashline_mismatch_message(path, expected_tag, actual_tag, file_lines, anchor_lines, hash_recognized=True):
    """Build a rich mismatch error message with surrounding context."""
    if not hash_recognized:
        return (
            "Edit rejected for %s: hash #%s is not from this session. "
            "The current file hashes to #%s. Re-read the file with read_file to copy a current "
            "[path#tag] header — never invent the tag and never reuse one from a prior session."
            % (path, expected_tag, actual_tag)
        )
    msg = (
        "Edit rejected for %s: file changed between read and edit. "
        "Section is bound to #%s; the current file hashes to #%s. "
        "Re-read the file with read_file to get a fresh [path#tag] header."
        % (path, expected_tag, actual_tag)
    )
    context = _hashline_format_anchor_context(anchor_lines, file_lines)
    if context:
        msg += "\n\nContext around the anchors:\n" + "\n".join(context)
    return msg


# ─── Contamination detection ───────────────────────────────────────────────

def _hashline_detect_contamination(text):
    """Detect apply_patch/unified-diff contamination in a line; return error or None."""
    trimmed = text.strip()
    if not trimmed:
        return None
    if (trimmed.startswith("*** Update File:")
        or trimmed.startswith("*** Add File:")
        or trimmed.startswith("*** Delete File:")
        or trimmed.startswith("*** Move to:")):
        preview = trimmed[:48] + ("..." if len(trimmed) > 48 else "")
        return (
            'apply_patch sentinel "%s" is not valid in hashline. '
            'File sections start with "[path#HASH]". '
            "Use SWAP N.=M:, DEL N.=M, or INS.PRE|POST|HEAD|TAIL: ops."
            % preview
        )
    if _re.match(r"^@@\s+[-+]?\d+,\d+\s+[-+]?\d+,\d+\s+@@", trimmed):
        return (
            "unified-diff hunk header (@@ -N,M +N,M @@) is not valid in hashline. "
            "Use SWAP N.=M:, DEL N.=M, or INS.PRE|POST|HEAD|TAIL: ops."
        )
    if trimmed.startswith("@@"):
        preview = trimmed[:48] + ("..." if len(trimmed) > 48 else "")
        return (
            '"@@"-bracketed hunk header "%s" is not valid in hashline. '
            "Drop the @@ ... @@ brackets and write a verb header such as SWAP N.=M:."
            % preview
        )
    # DEL with colon and body — a common mistake
    if _re.match(r"^DEL\s+[1-9]\d*(?:\s*(?:\.\.|\u2026|-|\s)\s*[1-9]\d*)?\s*:", trimmed):
        return "DEL N.=M has no colon and no body. Remove the colon and body rows."
    # Bare number — needs a verb
    if _re.match(r"^[1-9]\d*$", trimmed):
        return (
            "hunk headers need a verb. Use SWAP %s.=%s: to replace, or DEL %s to delete."
            % (trimmed, trimmed, trimmed)
        )
    # Bare range without verb
    bare = _re.match(r"^([1-9]\d*)\s*[-.\u2026 =]+\s*([1-9]\d*)\s*:?$", trimmed)
    if bare:
        return (
            'bare range hunk header "%s" is not valid. '
            "Hunk headers need a verb: write SWAP %s.=%s: or DEL %s.=%s."
            % (trimmed, bare.group(1), bare.group(2), bare.group(1), bare.group(2))
        )
    return None


# ─── apply_patch path-noise stripping ──────────────────────────────────────

_APPLY_PATCH_PATH_NOISE_RE = _re.compile(
    r"^\*{0,3}\s*(?:(?:update|add|delete|move)[^A-Za-z0-9]*(?:file|to)?[^A-Za-z0-9]*:)?\s*\*{0,3}\s*",
    _re.IGNORECASE,
)


def _hashline_strip_path_noise(path_text):
    return _APPLY_PATCH_PATH_NOISE_RE.sub("", path_text)


# ─── Prefix stripping for echoed payloads ──────────────────────────────────

_HL_PREFIX_RE = _re.compile(r"^\s*(?:>>>|>>)?\s*(?:[+*-]\s*)?\d+:")
_HL_HEADER_RE = _re.compile(r"^\[[^#\r\n]+#[0-9A-Fa-f]{4}\]\s*$")
_READ_TRUNCATION_RE = _re.compile(
    r"^\[(?:Showing lines \d+-\d+ of \d+|\d+ more lines? in (?:file|\S+))\b.*\b"
)


def _hashline_strip_one_prefix(line):
    """Strip at most one hashline line-number prefix (N:, >>>N:, +N:, etc.)."""
    return _HL_PREFIX_RE.sub("", line)


def _hashline_should_strip_prefixes(lines):
    """Check if body rows are prefixed with hashline line numbers; return (strip_bool, is_plus_form)."""
    non_empty = [l for l in lines if l.strip()]
    if not non_empty:
        return False, False
    hash_prefixed = sum(1 for l in non_empty if _HL_PREFIX_RE.match(l))
    content_lines = [l for l in non_empty if not _HL_HEADER_RE.match(l.strip())]
    if content_lines and hash_prefixed == len(content_lines):
        return True, False
    plus_hash = sum(1 for l in non_empty if _re.match(r"^\s*(?:>>>|>>)?\s*\+\s*\d+:", l))
    if plus_hash > 0:
        return True, True
    return False, False


def _hashline_parse_payload_text(text):
    """Normalize a single payload line: if it's a bare row (no + prefix), auto-pipe it."""
    return text


# ─── Header / patch parsing ─────────────────────────────────────────────────

BEGIN_PATCH = "*** Begin Patch"
END_PATCH = "*** End Patch"
ABORT_MARKER = "*** Abort"

# Alternate range separators accepted (in addition to .=)
_RANGE_SEPARATORS = r"\.=|\.\.|\u2026|-| "


def _hashline_header_re():
    # Accept optional apply_patch noise before the bracket; path may contain spaces
    return _re.compile(r"^\[([^#\r\n]+)#([0-9A-Fa-f]{4})\]\s*$")


def _hashline_header_re_lenient():
    """Loose header: optional #TAG, with path-noise stripping, spaces allowed in path."""
    return _re.compile(r"^\[(.+)\]\s*$")


def _hashline_try_recover_header(line):
    """Best-effort recovery for bracketed headers with apply_patch noise or missing tag."""
    if not (line.startswith("[") and line.endswith("]")):
        return None
    body = _hashline_strip_path_noise(line[1:-1].strip())
    if not body:
        return None
    # Trailing #XXXX is the tag (path may contain spaces)
    trailing = _re.search(r"#([0-9A-Fa-f]{4})\s*$", body)
    if trailing:
        path_text = body[: trailing.start()]
        tag = trailing.group(1).upper()
    else:
        path_text = body.rstrip()
        tag = None
    if "#" in path_text:
        return None
    path = path_text.strip()
    if not path:
        return None
    return {"path": path, "tag": tag}


def _hashline_parse_patch(patch):
    lines = str(patch).replace("\r\n", "\n").replace("\r", "\n").split("\n")
    # Skip leading blanks and optional envelope start
    i = 0
    while i < len(lines) and (not lines[i].strip() or lines[i].strip() == BEGIN_PATCH):
        i += 1
    sections = []
    warnings = []
    header_rx = _hashline_header_re()
    loose_rx = _hashline_header_re_lenient()
    while i < len(lines):
        line = lines[i].strip()
        if not line:
            i += 1
            continue
        if line == END_PATCH or line == ABORT_MARKER:
            break
        # Try strict header first
        m = header_rx.match(line)
        if m:
            path = _hashline_strip_path_noise(m.group(1))
            tag = m.group(2).upper()
        else:
            # Try lenient recovery
            recovered = _hashline_try_recover_header(lines[i].strip())
            if recovered:
                path = recovered["path"]
                tag = recovered["tag"]
                if tag is None:
                    raise ValueError(
                        'input must begin with "[PATH#HASH]" on the first non-blank patch line; '
                        'Example: "[src/foo.ts#1A2B]"\nGot: %r' % lines[i]
                    )
            else:
                # Check for contamination
                contamination = _hashline_detect_contamination(lines[i].strip())
                if contamination:
                    raise ValueError(contamination)
                raise ValueError(
                    'input must begin with "[PATH#HASH]" on the first non-blank patch line; '
                    'Example: "[src/foo.ts#1A2B]"\nGot: %r' % lines[i]
                )
        section = {"path": path, "tag": tag, "ops": [], "warnings": []}
        i += 1
        while i < len(lines):
            raw = lines[i]
            stripped = raw.strip()
            if stripped == END_PATCH or stripped == ABORT_MARKER:
                i = len(lines)
                break
            if not stripped:
                i += 1
                continue
            if header_rx.match(stripped) or loose_rx.match(stripped):
                # Check if it's another section header
                if header_rx.match(stripped) or _hashline_try_recover_header(stripped):
                    break
            # Check for contamination in op lines
            contamination = _hashline_detect_contamination(stripped)
            if contamination:
                raise ValueError(contamination)

            def take_payload():
                nonlocal i
                payload = []
                i += 1
                while i < len(lines) and lines[i].startswith("+"):
                    payload.append(lines[i][1:])
                    i += 1
                # Handle bare (non-+) body rows: auto-pipe with warning
                while i < len(lines) and lines[i].strip() and not lines[i].strip().startswith(
                    ("[", "SWAP", "DEL", "INS", "***")
                ):
                    bare_line = lines[i]
                    # Reject minus rows (unified-diff style)
                    if bare_line.lstrip().startswith("-") and not bare_line.lstrip().startswith("+-"):
                        raise ValueError(
                            "`-` rows are not valid; the range already names the lines being changed. "
                            "For a literal `-` line, write `+-...`."
                        )
                    # Check if it's a read-output prefixed row
                    if _HL_PREFIX_RE.match(bare_line):
                        stripped_bare = _hashline_strip_one_prefix(bare_line)
                        payload.append(stripped_bare)
                        section["warnings"].append(
                            "Auto-prefixed bare body row(s) with +. Body rows must be +TEXT literal lines."
                        )
                    elif bare_line.startswith("+"):
                        payload.append(bare_line[1:])
                    else:
                        payload.append(bare_line)
                        section["warnings"].append(
                            "Auto-prefixed bare body row(s) with +. Body rows must be +TEXT literal lines."
                        )
                    i += 1
                return payload

            # SWAP with range (.=, .., …, -, or space separator) and optional colon
            m = _re.match(
                r"^SWAP\s+(\d+)\s*(?:%s)?\s*(\d+)?\s*:?$" % _RANGE_SEPARATORS, stripped
            )
            if m:
                start = int(m.group(1))
                end = int(m.group(2)) if m.group(2) else start
                payload = take_payload()
                if not payload:
                    raise ValueError("SWAP %d.=%d needs at least one +TEXT payload line" % (start, end))
                section["ops"].append({"kind": "swap", "start": start, "end": end, "payload": payload})
                continue
            # SWAP.BLK with optional colon
            m = _re.match(r"^SWAP\.BLK\s+(\d+)\s*:?$", stripped)
            if m:
                payload = take_payload()
                if not payload:
                    raise ValueError("SWAP.BLK %s needs at least one +TEXT payload line" % m.group(1))
                section["ops"].append({"kind": "swap_block", "anchor": int(m.group(1)), "payload": payload})
                continue
            # DEL with range and optional separator/colon
            m = _re.match(
                r"^DEL\s+(\d+)\s*(?:%s)?\s*(\d+)?\s*$" % _RANGE_SEPARATORS, stripped
            )
            if m:
                start = int(m.group(1))
                end = int(m.group(2)) if m.group(2) else start
                section["ops"].append({"kind": "delete", "start": start, "end": end})
                i += 1
                continue
            # DEL.BLK
            m = _re.match(r"^DEL\.BLK\s+(\d+)\s*$", stripped)
            if m:
                section["ops"].append({"kind": "delete_block", "anchor": int(m.group(1))})
                i += 1
                continue
            # INS.PRE/POST with optional colon
            m = _re.match(r"^INS\.(PRE|POST)\s+(\d+)\s*:?$", stripped)
            if m:
                payload = take_payload()
                if not payload:
                    raise ValueError("INS.%s %s needs at least one +TEXT payload line" % (m.group(1), m.group(2)))
                section["ops"].append(
                    {"kind": "insert", "where": m.group(1), "anchor": int(m.group(2)), "payload": payload}
                )
                continue
            # INS.BLK.POST with optional colon
            m = _re.match(r"^INS\.BLK\.POST\s+(\d+)\s*:?$", stripped)
            if m:
                payload = take_payload()
                if not payload:
                    raise ValueError("INS.BLK.POST %s needs at least one +TEXT payload line" % m.group(1))
                section["ops"].append(
                    {"kind": "insert_block_post", "anchor": int(m.group(1)), "payload": payload}
                )
                continue
            # INS.HEAD/TAIL with optional colon
            m = _re.match(r"^INS\.(HEAD|TAIL)\s*:?$", stripped)
            if m:
                payload = take_payload()
                if not payload:
                    raise ValueError("INS.%s needs at least one +TEXT payload line" % m.group(1))
                section["ops"].append(
                    {"kind": "insert_edge", "where": m.group(1), "payload": payload}
                )
                continue
            # Reject minus rows
            if stripped.startswith("-"):
                raise ValueError(
                    "`-` rows are not valid; the range already names the lines being changed. "
                    "For a literal `-` line, write `+-...`."
                )
            raise ValueError("unrecognized hashline op: %r" % raw)
        if not section["ops"]:
            raise ValueError("section for %s contains no edit ops" % section["path"])
        sections.append(section)
    if not sections:
        raise ValueError("patch contained no hashline sections")
    # Collect warnings from all sections
    for s in sections:
        warnings.extend(s.get("warnings", []))
    return sections


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


# ─── Apply concrete ops ─────────────────────────────────────────────────────

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


# ─── Prepare section (BOM, line endings, atomic write) ─────────────────────

def _hashline_prepare_section(section):
    path = section["path"]
    with open(path, "r", errors="replace", newline="") as f:
        raw_text = f.read()
    # BOM stripping and line-ending detection
    bom, text_no_bom = _hashline_strip_bom(raw_text)
    line_ending = _hashline_detect_line_ending(text_no_bom)
    live_text = _hashline_normalize_to_lf(text_no_bom)
    live_lines = _hashline_lines(live_text)
    live_tag = _hashline_compute_tag(live_text)
    expected_tag = section["tag"]
    snapshot = _hashline_find_snapshot(path, expected_tag)
    warnings = list(section.get("warnings", []))

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
            # Rich mismatch message with context
            anchor_lines = []
            for op in section["ops"]:
                if "start" in op:
                    anchor_lines.append(op["start"])
                if "anchor" in op:
                    anchor_lines.append(op["anchor"])
            raise ValueError(
                _hashline_mismatch_message(
                    path, expected_tag, live_tag, live_lines, anchor_lines, hash_recognized=False
                )
            )
        base_lines = _hashline_lines(snapshot["text"])
        target_lines = live_lines
        line_map = _hashline_build_equal_line_map(base_lines, live_lines)
        warnings.append("Recovered from stale hash tag using snapshot-based line remapping.")

    ops = _hashline_lower_ops(section, snapshot, target_lines, line_map)
    after_text = _hashline_apply_concrete_ops(target_lines, ops)
    # Collect repair warnings from section
    warnings.extend(section.get("warnings", []))
    # Noop detection at section level
    if after_text == live_text:
        warnings.append("Section produced no change (noop).")
    return {
        "path": path,
        "canonical_path": _hashline_canonical_path(path),
        "before": live_text,
        "after": after_text,
        "warnings": warnings,
        "bom": bom,
        "line_ending": line_ending,
    }
