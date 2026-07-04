import unittest

from stats import mean, median


class StatsTest(unittest.TestCase):
    def test_mean(self):
        self.assertEqual(mean([1, 2, 3, 4]), 2.5)

    def test_median_odd(self):
        self.assertEqual(median([9, 1, 5]), 5)

    def test_median_even(self):
        self.assertEqual(median([7, 1, 3, 5]), 4)

    def test_median_sorted_input(self):
        self.assertEqual(median([1, 2, 3]), 2)


if __name__ == "__main__":
    unittest.main()
