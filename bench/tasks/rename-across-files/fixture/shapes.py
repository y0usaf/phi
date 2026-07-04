from geometry import calc_area


class Rectangle:
    def __init__(self, width, height):
        self.width = width
        self.height = height

    def area(self):
        return calc_area(self.width, self.height)
