def valid_query_fields(query, allowed: set[str], *, required=()) -> bool:
    """Check scalar query fields before reading values; routes own value/error policy."""
    return (
        set(required) <= query.keys() <= allowed
        and all(len(values) == 1 for _, values in query.lists())
    )
