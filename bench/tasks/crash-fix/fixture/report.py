import json
import sys


def total(path):
    with open(path) as f:
        data = json.load(f)
    return sum(order["amount"] for order in data["orders"])


def main():
    print(f"TOTAL {total(sys.argv[1]):.2f}")


if __name__ == "__main__":
    main()
