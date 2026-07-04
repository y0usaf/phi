import unittest

from geometry import calc_area
from report import format_report
from shapes import Rectangle


class ShapesTest(unittest.TestCase):
    def test_calc_area(self):
        self.assertEqual(calc_area(6, 7), 42)

    def test_rectangle_area(self):
        self.assertEqual(Rectangle(3, 4).area(), 12)

    def test_report(self):
        self.assertEqual(format_report(2, 5), "area=10 perimeter=14")


if __name__ == "__main__":
    unittest.main()
