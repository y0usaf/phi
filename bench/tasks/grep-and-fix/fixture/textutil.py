def truncate(text, limit):
    """Truncate to `limit` characters, ellipsis included."""
    if len(text) <= limit:
        return text
    return text[: limit - 1] + "…"


def pad(text, width):
    return text + " " * max(0, width - len(text))
