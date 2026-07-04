import sys

from pager import paginate
from textutil import truncate


def main():
    items = [line.rstrip("\n") for line in sys.stdin]
    page = int(sys.argv[1]) if len(sys.argv) > 1 else 0
    for item in paginate(items, page, 20):
        print(truncate(item, 80))


if __name__ == "__main__":
    main()
