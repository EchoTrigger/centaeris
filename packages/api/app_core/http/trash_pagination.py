import base64
import binascii
import json


TRASH_PAGE_SIZE = 50


def read_trash_cursor(
    request,
    kind: str,
    fields: set[str],
    allowed_query_fields: set[str] | None = None,
) -> dict | None:
    query_fields = set(request.GET.keys())
    if query_fields - (allowed_query_fields or {"cursor"}) or any(
        len(request.GET.getlist(field)) != 1 for field in query_fields
    ):
        raise ValueError("trash_query_invalid")
    raw_cursor = request.GET.get("cursor")
    if raw_cursor is None:
        return None
    try:
        padding = "=" * (-len(raw_cursor) % 4)
        payload = json.loads(
            base64.b64decode(
                f"{raw_cursor}{padding}",
                altchars=b"-_",
                validate=True,
            ).decode("utf-8")
        )
        if (
            not isinstance(payload, dict)
            or set(payload) != {"kind", *fields}
            or payload["kind"] != kind
        ):
            raise ValueError
        return payload
    except (binascii.Error, TypeError, ValueError, UnicodeDecodeError):
        raise ValueError("trash_cursor_invalid") from None


def trash_page(queryset, kind: str, cursor_fields) -> tuple[list, str | None, bool]:
    page = list(queryset[: TRASH_PAGE_SIZE + 1])
    has_more = len(page) > TRASH_PAGE_SIZE
    page = page[:TRASH_PAGE_SIZE]
    if not has_more:
        return page, None, False
    payload = {"kind": kind, **cursor_fields(page[-1])}
    next_cursor = encode_trash_cursor(payload)
    return page, next_cursor, True


def encode_trash_cursor(payload: dict) -> str:
    return base64.urlsafe_b64encode(
        json.dumps(payload, separators=(",", ":")).encode("utf-8")
    ).decode("ascii").rstrip("=")
