set -e
out=$(python3 wordcount.py data.csv)
test "$out" = "4 4 45"
printf 'alpha beta\ngamma\n' > .check_tmp.txt
out=$(python3 wordcount.py .check_tmp.txt)
test "$out" = "2 3 17"
