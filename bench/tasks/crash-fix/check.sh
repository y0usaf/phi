set -e
out=$(python3 report.py sales.json)
test "$out" = "TOTAL 1234.50"
