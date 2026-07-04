set -e
python3 - <<'EOF'
text = open("CHANGELOG.md").read()
lines = [l.rstrip() for l in text.splitlines()]

# Section heading order.
headings = [l for l in lines if l.startswith("## ")]
assert headings == [
    "## [Unreleased]",
    "## [1.2.0] - 2026-07-01",
    "## [1.1.0] - 2026-05-14",
    "## [1.0.0] - 2026-04-01",
], headings

def section(name):
    start = lines.index(name)
    end = next(
        (i for i in range(start + 1, len(lines)) if lines[i].startswith("## ")),
        len(lines),
    )
    return lines[start + 1 : end]

unreleased = section("## [Unreleased]")
assert not any(l.startswith("- ") for l in unreleased), unreleased

new = section("## [1.2.0] - 2026-07-01")
subs = [l for l in new if l.startswith("### ")]
assert subs == ["### Added", "### Changed"], subs
added = new[new.index("### Added") + 1 : new.index("### Changed")]
assert [l for l in added if l.startswith("- ")] == [
    "- config hot-reload",
    "- verbose flag",
], added
changed = new[new.index("### Changed") + 1 :]
assert [l for l in changed if l.startswith("- ")] == ["- rework config loading"], changed

# The 1.1.0 and 1.0.0 sections are untouched.
assert "- `--json` output flag" in text
assert "- crash on empty input files" in text
assert "- initial release" in text
EOF
