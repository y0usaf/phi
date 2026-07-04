set -e
cat > wordcount.py <<'EOF'
import sys

with open(sys.argv[1], "rb") as f:
    data = f.read()
text = data.decode("utf-8", errors="replace")
print(f"{text.count(chr(10))} {len(text.split())} {len(data)}")
EOF
