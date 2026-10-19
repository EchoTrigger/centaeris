"""Maintenance-only host profile registration, independent of Run authority."""
import secrets

from django.conf import settings
from django.http import JsonResponse
from ninja import Router

from app_core.environment_profile import EnvironmentProfileError, bind_environment_profile
from .json_body import decode_json_object

router = Router(tags=['internal'], by_alias=True)


def _authorized(request):
    configured = getattr(settings, 'ENVIRONMENT_MAINTENANCE_TOKEN', '')
    supplied = request.headers.get('x-environment-maintenance-token', '')
    return (isinstance(configured, str) and len(configured) >= 32
            and configured != settings.INTERNAL_API_TOKEN
            and isinstance(supplied, str) and secrets.compare_digest(configured, supplied))


@router.post('/environments/{environment_id}/environment-profile', auth=None,
             response=None, include_in_schema=False)
def bind_session_environment_profile(request, environment_id: str):
    if not _authorized(request):
        return JsonResponse({'error': 'environment_maintenance_authority_required'}, status=403)
    try:
        body = decode_json_object(request)
        result = bind_environment_profile(environment_id, body)
    except EnvironmentProfileError as error:
        return JsonResponse({'error': error.code}, status=error.status)
    except ValueError:
        return JsonResponse({'error': 'environment_profile_request_invalid'}, status=400)
    return JsonResponse(result)
