import unittest

from pager import paginate


class PagerTest(unittest.TestCase):
    def test_first_page(self):
        self.assertEqual(paginate(list(range(10)), 0, 3), [0, 1, 2])

    def test_middle_page(self):
        self.assertEqual(paginate(list(range(10)), 2, 3), [6, 7, 8])

    def test_short_last_page(self):
        self.assertEqual(paginate(list(range(10)), 3, 3), [9])

    def test_empty(self):
        self.assertEqual(paginate([], 0, 3), [])


if __name__ == "__main__":
    unittest.main()
