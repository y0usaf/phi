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


