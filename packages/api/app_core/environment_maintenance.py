"""Independent, durable maintenance ownership on the ordinary Environment lock.

Lock order: Workspace, Session, Environment, operation, grant. Physical allocation
pins use NOWAIT: a Runtime allocation holder may itself be waiting on Environment.
Database allocation identity is not a claim that the container is still alive.
"""
import hashlib
import json
import re
import secrets

from django.conf import settings
from django.db import connection
from django.utils import timezone

from .environment_lifecycle import MAX_CONTROLLER_EPOCH, _translate_contention, controller_lease_digest
from .environment_maintenance_authorization import SCHEMA, sign_maintenance_authorization, sign_provision_authorization
from .models import EnvironmentMaintenanceGrant, EnvironmentMaintenanceOperation, Session, SessionEnvironment, Workspace

LEASE_MS = 5 * 60 * 1000
MAX_COMMAND_TIMEOUT_MS = 3_600_000
CLEANUP_GRACE_MS = 30_000


class MaintenanceError(ValueError):
    def __init__(self, code, status=409):
        super().__init__(code)
        self.code, self.status = code, status


def _now_ms():
    with connection.cursor() as cursor:
        cursor.execute('SELECT (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint')
        return cursor.fetchone()[0]


def _digest(request):
    if not isinstance(request, dict) or not request:
        raise MaintenanceError('maintenance_request_invalid', 400)
    try:
        encoded = json.dumps(request, ensure_ascii=False, allow_nan=False, sort_keys=True, separators=(',', ':')).encode()
    except (TypeError, ValueError, RecursionError):
        raise MaintenanceError('maintenance_request_invalid', 400) from None
    if len(encoded) > 64 * 1024:
        raise MaintenanceError('maintenance_request_invalid', 400)
    return 'sha256:' + hashlib.sha256(encoded).hexdigest()


def _maintenance_lease_ms(request):
    # Command duration is part of the immutable signed request. The server sets
    # the lease once; replay returns its original deadline without extension.
    if isinstance(request, dict) and set(request) == {'program', 'args', 'cwd', 'timeoutMs'}:
        if (type(request['timeoutMs']) is not int or not 0 < request['timeoutMs'] <= MAX_COMMAND_TIMEOUT_MS
                or any(not isinstance(request[name], str) or not request[name] or '\x00' in request[name]
                    for name in ('program', 'cwd'))
                or not isinstance(request['args'], list)
                or any(not isinstance(arg, str) or '\x00' in arg for arg in request['args'])):
            raise MaintenanceError('maintenance_request_invalid', 400)
        return max(LEASE_MS, request['timeoutMs'] + CLEANUP_GRACE_MS)
    if (isinstance(request, dict) and set(request) == {'request', 'contentLength', 'contentDigest'}
            and isinstance(request['request'], dict) and type(request['contentLength']) is int
            and 0 <= request['contentLength'] <= 32 * 1024 * 1024
            and isinstance(request['contentDigest'], str)
            and re.fullmatch(r'sha256:[0-9a-f]{64}', request['contentDigest'])):
        return LEASE_MS
    raise MaintenanceError('maintenance_request_invalid', 400)


def _locked_environment(environment_id):
    initial = SessionEnvironment.objects.filter(pk=environment_id).values('session_id').first()
    if initial is None:
        raise MaintenanceError('maintenance_environment_missing', 404)
    workspace_id = Session.objects.values_list('workspace_id', flat=True).get(pk=initial['session_id'])
    workspace = Workspace.objects.select_for_update().get(pk=workspace_id)
    session = Session.objects.select_for_update(of=('self',)).get(pk=initial['session_id'])
    environment = SessionEnvironment.objects.select_for_update().get(pk=environment_id)
    if environment.session_id != session.pk or session.workspace_id != workspace.pk:
        raise MaintenanceError('maintenance_environment_binding_invalid')
    return workspace, session, environment


def _allocation(workspace, session, environment):
    with connection.cursor() as cursor:
        cursor.execute('SELECT execution_id FROM runtime.execution_instances '
            'WHERE environment_id=%s AND session_id=%s AND execution_id=allocation_execution_id '
            "AND allocation_mode='retained' LIMIT 2 FOR SHARE NOWAIT", [environment.pk, session.pk])
        if len(cursor.fetchall()) != 1:
            raise MaintenanceError('maintenance_allocation_unavailable')
        cursor.execute('SELECT i.instance_id,s.container_id,i.host_id,i.execution_id,i.profile_digest '
            'FROM runtime.execution_instances i JOIN runtime.resident_sandboxes s '
            'ON s.execution_id=i.execution_id AND s.instance_id=i.instance_id '
            'AND s.environment_id=i.environment_id AND s.host_id=i.host_id '
            'WHERE i.environment_id=%s AND i.session_id=%s AND s.workspace_id=%s '
            "AND i.execution_id=i.allocation_execution_id AND i.allocation_mode='retained' "
            "AND s.allocation_mode='retained' AND s.state='resident' "
            'LIMIT 2 FOR SHARE OF i,s NOWAIT', [environment.pk, session.pk, workspace.pk])
        rows = cursor.fetchall()
        if len(rows) != 1:
            raise MaintenanceError('maintenance_allocation_unavailable')
        cursor.execute('SELECT 1 FROM runtime.environment_retirements WHERE environment_id=%s FOR SHARE NOWAIT',
            [environment.pk])
        if cursor.fetchone() is not None:
            raise MaintenanceError('maintenance_environment_retiring')
    instance, container, host, execution, profile = rows[0]
    from .environment_profile import environment_profile_digest
    if (environment.environment_profile is None
            or environment_profile_digest(environment.environment_profile) != environment.environment_profile_digest
            or profile != environment.environment_profile_digest):
        raise MaintenanceError('maintenance_allocation_profile_mismatch')
    if (not isinstance(instance, str) or not re.fullmatch(r'instance_[0-9a-f]{64}', instance)
            or not isinstance(container, str) or not re.fullmatch(r'[0-9a-f]{64}', container)
            or not isinstance(profile, str) or not re.fullmatch(r'sha256:[0-9a-f]{64}', profile)
            or not isinstance(host, str) or not host or not isinstance(execution, str) or not execution):
        raise MaintenanceError('maintenance_allocation_unavailable')
    return dict(instance_id=instance, container_id=container, host_id=host,
                allocation_execution_id=execution, profile_digest=profile)


def _grant_result(grant):
    signer = sign_provision_authorization if grant.operation.operation_kind == 'provision' else sign_maintenance_authorization
    try:
        valid = (_digest(grant.payload) == grant.digest
            and secrets.compare_digest(signer(grant.payload, settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY), grant.signature))
    except (ValueError, TypeError, KeyError):
        valid = False
    if not valid:
        raise MaintenanceError('maintenance_grant_invalid')
    return {'payload': grant.payload, 'signature': grant.signature}


def _completion_matches(operation, grant):
    """None means no record; false is conflicting or malformed immutable evidence."""
    if operation.operation_kind == 'provision':
        from .environment_provision import provision_completion_matches
        return provision_completion_matches(operation, grant)
    with connection.cursor() as cursor:
        cursor.execute('SELECT environment_id,instance_id,container_id,profile_digest,controller_epoch,'
            'controller_lease_digest,request_digest,completion_json FROM runtime.environment_maintenance_completions '
            'WHERE operation_id=%s FOR SHARE NOWAIT', [operation.pk])
        row = cursor.fetchone()
    if row is None:
        return None
    expected = (operation.environment_id, operation.instance_id, operation.container_id, operation.profile_digest,
                grant.controller_epoch, grant.controller_lease_digest, operation.request_digest)
    if tuple(row[:7]) != expected:
        return False
    try:
        completion = json.loads(row[7])
        return (isinstance(completion, dict) and set(completion) == {'kind', 'taskIds', 'cleanupComplete'}
            and completion['kind'] == 'operationEnded' and completion['cleanupComplete'] is True
            and isinstance(completion['taskIds'], list)
            and all(isinstance(item, str) and 0 < len(item) <= 160 for item in completion['taskIds'])
            and len(completion['taskIds']) == len(set(completion['taskIds'])))
    except (TypeError, ValueError, RecursionError):
        return False


def issue_maintenance_authorization(environment_id, body):
    fields = {'schema', 'operationId', 'controllerPurpose', 'expectedControllerEpoch', 'request'}
    if (not isinstance(body, dict) or set(body) != fields
            or body['schema'] != 'workspace.environment_maintenance.issue.v1'
            or not isinstance(body['operationId'], str)
            or not re.fullmatch(r'[A-Za-z0-9_.:-]{1,160}', body['operationId'])
            or body['controllerPurpose'] not in ('execute', 'cleanup')
            or type(body['expectedControllerEpoch']) is not int
            or not 0 <= body['expectedControllerEpoch'] < MAX_CONTROLLER_EPOCH):
        raise MaintenanceError('maintenance_request_invalid', 400)
    request_digest = _digest(body['request'])
    lease_ms = _maintenance_lease_ms(body['request'])
    expected, purpose = body['expectedControllerEpoch'], body['controllerPurpose']

    def issue():
        workspace, session, environment = _locked_environment(environment_id)
        operation = EnvironmentMaintenanceOperation.objects.select_for_update().filter(pk=body['operationId']).first()
        if operation is not None:
            if (operation.operation_kind != 'task' or operation.environment_id != environment.pk or operation.workspace_id != workspace.pk
                    or operation.session_id != session.pk or operation.request_digest != request_digest
                    or operation.request != body['request']):
                raise MaintenanceError('maintenance_operation_conflict')
            replay = operation.grants.select_for_update().filter(
                previous_controller_epoch=expected, controller_purpose=purpose).first()
            if replay is not None:
                return _grant_result(replay)
            if purpose == 'execute' or operation.state != 'active':
                raise MaintenanceError('maintenance_operation_conflict')
        elif purpose != 'execute':
            raise MaintenanceError('maintenance_operation_missing')
        if (environment.controller_epoch != expected or not environment.controller_released
                and environment.controller_maintenance_operation_id != body['operationId']):
            raise MaintenanceError('maintenance_controller_owned')
        if purpose == 'execute':
            if (not environment.controller_released or workspace.status != 'active'
                    or session.deletedAt is not None or session.current_environment_id != environment.pk):
                raise MaintenanceError('maintenance_controller_owned')
        else:
            if environment.controller_released or environment.controller_maintenance_operation_id != operation.pk:
                raise MaintenanceError('maintenance_controller_owned')
            current = EnvironmentMaintenanceGrant.objects.select_for_update().get(pk=operation.current_grant_id)
            completion = _completion_matches(operation, current)
            if completion is not None:
                raise MaintenanceError('maintenance_completion_available' if completion else 'maintenance_completion_conflict')
            if current.expires_at_ms > _now_ms():
                raise MaintenanceError('maintenance_cleanup_recovery_not_due')
        target = _allocation(workspace, session, environment)
        if operation is None:
            operation = EnvironmentMaintenanceOperation.objects.create(operation_id=body['operationId'],
                workspace=workspace, session=session, environment=environment, request=body['request'],
                request_digest=request_digest, **target)
        elif any(getattr(operation, name) != value for name, value in target.items()):
            raise MaintenanceError('maintenance_operation_target_changed')
        lease_owner = 'maintenance:' + secrets.token_hex(32)
        lease_digest = controller_lease_digest(lease_owner)
        epoch, deadline = environment.controller_epoch + 1, _now_ms() + (lease_ms if purpose == 'execute' else LEASE_MS)
        payload = {'schema': SCHEMA, 'operationKind': 'task', 'workspaceId': workspace.pk, 'sessionId': session.pk,
            'environmentId': environment.pk, 'instanceId': operation.instance_id,
            'containerId': operation.container_id, 'profileDigest': operation.profile_digest,
            'controllerOwner': {'kind': 'maintenanceOperation', 'operationId': operation.pk},
            'controllerEpoch': epoch, 'controllerLeaseDigest': lease_digest,
            'controllerPurpose': purpose, 'requestDigest': request_digest, 'expiresAtMs': deadline}
        signature = sign_maintenance_authorization(payload, settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY)
        grant = EnvironmentMaintenanceGrant.objects.create(operation=operation,
            previous_controller_epoch=expected, controller_epoch=epoch, controller_lease_digest=lease_digest,
            controller_purpose=purpose, expires_at_ms=deadline, payload=payload, signature=signature)
        operation.current_grant = grant
        operation.save(update_fields=['current_grant'])
        environment.controller_epoch, environment.controller_run = epoch, None
        environment.controller_maintenance_operation = operation
        environment.controller_lease_owner, environment.controller_purpose = lease_owner, purpose
        environment.controller_released = False
        environment.save(update_fields=['controller_epoch', 'controller_run', 'controller_maintenance_operation',
            'controller_lease_owner', 'controller_purpose', 'controller_released'])
        return _grant_result(grant)
    return _translate_contention(issue)


def release_maintenance_operation(environment_id, operation_id, body):
    if (not isinstance(body, dict) or set(body) != {'schema', 'controllerEpoch', 'controllerLeaseDigest'}
            or body['schema'] != 'workspace.environment_maintenance.release.v1'
            or type(body['controllerEpoch']) is not int
            or not 1 <= body['controllerEpoch'] <= MAX_CONTROLLER_EPOCH
            or not isinstance(body['controllerLeaseDigest'], str)
            or not re.fullmatch(r'sha256:[0-9a-f]{64}', body['controllerLeaseDigest'])):
        raise MaintenanceError('maintenance_release_invalid', 400)

    def release():
        _, _, environment = _locked_environment(environment_id)
        operation = EnvironmentMaintenanceOperation.objects.select_for_update().filter(
            pk=operation_id, environment=environment).first()
        if operation is None or operation.current_grant_id is None:
            raise MaintenanceError('maintenance_operation_missing', 404)
        grant = EnvironmentMaintenanceGrant.objects.select_for_update().get(pk=operation.current_grant_id)
        _grant_result(grant)
        if grant.controller_epoch != body['controllerEpoch'] or grant.controller_lease_digest != body['controllerLeaseDigest']:
            raise MaintenanceError('maintenance_release_rejected')
        receipt = {'environmentId': environment.pk,
            'controllerOwner': {'kind': 'maintenanceOperation', 'operationId': operation.pk},
            'controllerEpoch': grant.controller_epoch, 'controllerLeaseDigest': grant.controller_lease_digest,
            'released': True}
        if operation.state == 'released':
            return receipt
        if (environment.controller_maintenance_operation_id != operation.pk or environment.controller_run_id is not None
                or environment.controller_epoch != grant.controller_epoch or environment.controller_released
                or controller_lease_digest(environment.controller_lease_owner) != grant.controller_lease_digest
                or environment.controller_purpose != grant.controller_purpose):
            raise MaintenanceError('maintenance_release_rejected')
        if _completion_matches(operation, grant) is not True:
            raise MaintenanceError('maintenance_completion_pending')
        environment.controller_released = True
        environment.save(update_fields=['controller_released'])
        operation.state, operation.released_at = 'released', timezone.now()
        operation.save(update_fields=['state', 'released_at'])
        return receipt
    return _translate_contention(release)
