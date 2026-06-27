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
