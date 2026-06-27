# ─── Hashing ───────────────────────────────────────────────────────────────

def _hashline_normalize_hash_text(text):
    return _re.sub(r"[ \t\r]+(?=\n|$)", "", text)


def _hashline_compute_tag(text):
    data = _hashline_normalize_hash_text(text).encode("utf-8", "surrogatepass")
    return "%04X" % (_zlib.crc32(data) & 0xFFFF)


