def slice_window(items, start, size):
    """Return up to `size` items starting at `start` (clamped)."""
    end = min(start + size - 1, len(items))
    return items[start:end]
