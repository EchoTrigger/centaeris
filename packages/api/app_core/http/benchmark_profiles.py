"""Maintenance-only host profile registration, independent of Run authority."""
import secrets

from django.conf import settings
from django.http import JsonResponse
from ninja import Router

from app_core.benchmark_profile import BenchmarkProfileError, bind_benchmark_profile, complete_benchmark_setup
from .json_body import decode_json_object

router = Router(tags=['internal'], by_alias=True)


def _authorized(request):
    configured = getattr(settings, 'ENVIRONMENT_MAINTENANCE_TOKEN', '')
    supplied = request.headers.get('x-environment-maintenance-token', '')
    return (isinstance(configured, str) and len(configured) >= 32
            and configured != settings.INTERNAL_API_TOKEN
            and isinstance(supplied, str) and secrets.compare_digest(configured, supplied))


@router.post('/environments/{environment_id}/benchmark-profile', auth=None,
             response=None, include_in_schema=False)
def bind_environment_benchmark_profile(request, environment_id: str):
    if not _authorized(request):
        return JsonResponse({'error': 'environment_maintenance_authority_required'}, status=403)
    try:
        body = decode_json_object(request)
        result = bind_benchmark_profile(environment_id, body)
    except BenchmarkProfileError as error:
        return JsonResponse({'error': error.code}, status=error.status)
    except ValueError:
        return JsonResponse({'error': 'benchmark_profile_request_invalid'}, status=400)
    return JsonResponse(result)


@router.post('/environments/{environment_id}/benchmark-setup-complete', auth=None,
             response=None, include_in_schema=False)
def complete_environment_benchmark_setup(request, environment_id: str):
    if not _authorized(request):
        return JsonResponse({'error': 'environment_maintenance_authority_required'}, status=403)
    try:
        result = complete_benchmark_setup(environment_id, decode_json_object(request))
    except BenchmarkProfileError as error:
        return JsonResponse({'error': error.code}, status=error.status)
    except ValueError:
        return JsonResponse({'error': 'benchmark_setup_request_invalid'}, status=400)
    return JsonResponse(result)
