set -e
if grep -rn "calc_area" --include="*.py" .; then
    echo "calc_area still present"
    exit 1
fi
grep -qn "def rectangle_area" geometry.py
python3 -m unittest discover -q
