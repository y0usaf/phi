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
    """Loose header: optional #TAG, spaces allowed in path."""
    return _re.compile(r"^\[(.+)\]\s*$")


def _hashline_try_recover_header(line):
    """Best-effort recovery for bracketed headers with apply_patch noise or missing tag."""
    if not (line.startswith("[") and line.endswith("]")):
        return None
    body = line[1:-1].strip()
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
            path = m.group(1).strip()
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


