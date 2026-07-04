set -e
python3 - <<'EOF'
import json

records = json.load(open("orders_out.json"))
source = json.load(open("orders.json"))
assert len(records) == len(source), "record count changed"
for rec, src in zip(records, source):
    for key, value in src.items():
        assert rec.get(key) == value, f"field {key} changed: {rec.get(key)!r}"
    assert round(rec["total"], 2) == round(src["qty"] * src["price"], 2), rec
EOF
