from windowing import slice_window


def paginate(items, page, page_size):
    """Items for the given zero-based page."""
    return slice_window(items, page * page_size, page_size)
