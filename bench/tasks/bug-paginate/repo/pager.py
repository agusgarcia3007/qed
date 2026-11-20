def paginate(items, page, per_page):
    """Return (page_items, total_pages). Pages are 1-indexed; a page out of range returns []."""
    total_pages = len(items) // per_page
    start = (page - 1) * per_page
    if page < 1 or page > total_pages:
        return [], total_pages
    return items[start:start + per_page], total_pages
