"""Independent maintenance issuance and completion-backed release."""
import secrets

from django.conf import settings
from django.db import DatabaseError
from django.http import JsonResponse
from ninja import Router

from app_core.environment_lifecycle import EnvironmentControllerBusy
from app_core.environment_maintenance import MaintenanceError, issue_maintenance_authorization, release_maintenance_operation
from .json_body import decode_json_object

router = Router(tags=['internal'], by_alias=True)


def _authorized(request):
    configured = getattr(settings, 'ENVIRONMENT_MAINTENANCE_TOKEN', '')
    supplied = request.headers.get('x-environment-maintenance-token', '')
    return (isinstance(configured, str) and len(configured) >= 32
        and configured != settings.INTERNAL_API_TOKEN and isinstance(supplied, str)
        and secrets.compare_digest(configured.encode('utf-8'), supplied.encode('utf-8')))


def _response(request, operation):
    if not _authorized(request):
        return JsonResponse({'error': 'environment_maintenance_authority_required'}, status=403)
    try:
        return JsonResponse(operation(decode_json_object(request)))
    except MaintenanceError as error:
        return JsonResponse({'error': error.code}, status=error.status)
    except EnvironmentControllerBusy:
        return JsonResponse({'error': 'environment_controller_busy'}, status=409)
    except DatabaseError:
        return JsonResponse({'error': 'maintenance_store_unavailable'}, status=503)
    except ValueError:
        return JsonResponse({'error': 'maintenance_request_invalid'}, status=400)


@router.post('/environments/{environment_id}/maintenance-authorizations', auth=None,
             response=None, include_in_schema=False)
def issue_environment_maintenance_authorization(request, environment_id: str):
    return _response(request, lambda body: issue_maintenance_authorization(environment_id, body))


@router.post('/environments/{environment_id}/provision-authorizations', auth=None,
             response=None, include_in_schema=False)
def issue_environment_provision_authorization(request, environment_id: str):
    from app_core.environment_provision import issue_provision_authorization
    return _response(request, lambda body: issue_provision_authorization(environment_id, body))


@router.post('/environments/{environment_id}/maintenance-operations/{operation_id}/release', auth=None,
             response=None, include_in_schema=False)
def release_environment_maintenance_operation(request, environment_id: str, operation_id: str):
    return _response(request, lambda body: release_maintenance_operation(environment_id, operation_id, body))
