set -e
python3 - <<'EOF'
from pathlib import Path

expected_timeout = {"alpha": 30, "beta": 60, "gamma": 30, "delta": 45, "epsilon": 30}
files = sorted(Path("services").glob("*.conf"))
assert [f.stem for f in files] == sorted(expected_timeout), files
for f in files:
    settings = {}
    for line in f.read_text().splitlines():
        if "=" in line:
            key, _, value = line.partition("=")
            settings[key.strip()] = value.strip()
    assert settings.get("debug") == "false", (f.name, settings)
    assert settings.get("timeout") == str(expected_timeout[f.stem]), (f.name, settings)
    assert settings.get("name") == f.stem, (f.name, settings)
    assert "port" in settings, (f.name, settings)
EOF
