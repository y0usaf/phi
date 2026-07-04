set -e
python3 - <<'EOF'
import json

records = json.load(open("orders.json"))
for rec in records:
    rec["total"] = round(rec["qty"] * rec["price"], 2)
json.dump(records, open("orders_out.json", "w"), indent=2)
EOF
