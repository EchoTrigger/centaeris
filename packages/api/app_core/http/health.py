from django.db import connection
from ninja import Router

from .schema import HealthResponse


router = Router(tags=["health"], by_alias=True)


@router.get("/health", auth=None, response=HealthResponse)
def health(request):
    connection.ensure_connection()
    return {"status": "ready"}
