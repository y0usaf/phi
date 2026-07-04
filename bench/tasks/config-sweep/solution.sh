set -e
python3 - <<'EOF'
from pathlib import Path

for f in Path("services").glob("*.conf"):
    lines = f.read_text().splitlines()
    lines = [
        "debug = false" if line.split("=")[0].strip() == "debug" else line
        for line in lines
    ]
    if not any(line.split("=")[0].strip() == "timeout" for line in lines):
        lines.append("timeout = 30")
    f.write_text("\n".join(lines) + "\n")
EOF
