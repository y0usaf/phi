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


