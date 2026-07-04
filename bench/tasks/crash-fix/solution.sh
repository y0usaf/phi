set -e
sed -i 's/order\["amount"\]/order.get("amount", 0)/' report.py
