import geometry


def format_report(width, height):
    area = geometry.calc_area(width, height)
    perimeter = geometry.perimeter(width, height)
    return f"area={area} perimeter={perimeter}"
