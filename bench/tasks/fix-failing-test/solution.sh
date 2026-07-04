set -e
sed -i 's/ordered = list(values)/ordered = sorted(values)/' stats.py
