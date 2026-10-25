from django.db import connection
from ninja import Router

from .schema import HealthResponse


router = Router(tags=["health"], by_alias=True)


@router.get("/health", auth=None, response=HealthResponse)
def health(request):
    connection.ensure_connection()
    from django.http import JsonResponse
    from ..native_archive import NativeArchiveBlocked, require_native_archive_activation

    try:
        require_native_archive_activation()
    except NativeArchiveBlocked as error:
        return JsonResponse({"error": str(error)}, status=503)
    return {"status": "ready"}
