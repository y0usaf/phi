set -e
python3 - <<'EOF'
text = open("CHANGELOG.md").read()
text = text.replace(
    """## [Unreleased]

- rework config loading

## [1.1.0]""",
    """## [Unreleased]

## [1.2.0] - 2026-07-01

### Added

- config hot-reload
- verbose flag

### Changed

- rework config loading

## [1.1.0]""",
)
open("CHANGELOG.md", "w").write(text)
EOF
