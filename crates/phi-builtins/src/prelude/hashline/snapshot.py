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


