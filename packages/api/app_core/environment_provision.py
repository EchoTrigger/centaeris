"""Provision authority precedes physical allocation; completion is Runtime-owned."""
import json
import re
import secrets

from django.conf import settings
from django.db import connection

from .environment_lifecycle import MAX_CONTROLLER_EPOCH, _translate_contention, controller_lease_digest
from .environment_maintenance import LEASE_MS, MaintenanceError, _digest, _grant_result, _locked_environment, _now_ms
from .environment_maintenance_authorization import PROVISION_SCHEMA, sign_provision_authorization
from .environment_profile import E2B_PROFILE_SCHEMA, environment_profile_digest
from .models import EnvironmentMaintenanceGrant, EnvironmentMaintenanceOperation
from .plugin_catalog import plugin_activation_for_workspace, plugin_lifecycle_lock


def issue_provision_authorization(environment_id, body):
    if (not isinstance(body, dict) or set(body) != {'schema', 'operationId', 'sessionId', 'expectedControllerEpoch'}
            or body['schema'] != 'workspace.environment_provision.issue.v1'
            or any(not isinstance(body[field], str) or not re.fullmatch(r'[A-Za-z0-9_.:-]{1,160}', body[field])
                for field in ('operationId', 'sessionId'))
            or type(body['expectedControllerEpoch']) is not int
            or not 0 <= body['expectedControllerEpoch'] < MAX_CONTROLLER_EPOCH):
        raise MaintenanceError('provision_request_invalid', 400)

    def issue():
        workspace, session, environment = _locked_environment(environment_id)
        if session.pk != body['sessionId']:
            raise MaintenanceError('provision_environment_binding_invalid')
        operation = EnvironmentMaintenanceOperation.objects.select_for_update().filter(pk=body['operationId']).first()
        if operation is not None:
            if (operation.operation_kind != 'provision' or operation.environment_id != environment.pk
                    or operation.session_id != session.pk or operation.workspace_id != workspace.pk):
                raise MaintenanceError('maintenance_operation_conflict')
            grant = operation.grants.select_for_update().filter(previous_controller_epoch=body['expectedControllerEpoch'],
                controller_purpose='execute').first()
            if grant is None:
                raise MaintenanceError('maintenance_operation_conflict')
            return _grant_result(grant)
        if (workspace.status != 'active' or session.deletedAt is not None or session.current_environment_id != environment.pk
                or not environment.controller_released or environment.controller_epoch != body['expectedControllerEpoch']):
            raise MaintenanceError('maintenance_controller_owned')
        if (environment.environment_profile is None
                or environment_profile_digest(environment.environment_profile) != environment.environment_profile_digest):
            raise MaintenanceError('environment_profile_binding_required')
        with connection.cursor() as cursor:
            if environment.environment_profile['schema'] == E2B_PROFILE_SCHEMA:
                if environment.environment_profile['lifetime'] != 'retained':
                    raise MaintenanceError('provision_provider_retained_required')
                # Prepared and Unknown responsibilities also prevent a new
                # operation from allocating around the original attempt.
                for table in ('provider_creation_operations', 'provider_environment_bindings'):
                    cursor.execute(f'SELECT 1 FROM runtime.{table} WHERE environment_id=%s LIMIT 1 FOR SHARE NOWAIT',
                        [environment.pk])
                    if cursor.fetchone() is not None:
                        raise MaintenanceError('provision_allocation_exists')
            else:
                cursor.execute('SELECT 1 FROM runtime.execution_instances WHERE environment_id=%s LIMIT 1 FOR SHARE NOWAIT',
                    [environment.pk])
                if cursor.fetchone() is not None:
                    raise MaintenanceError('provision_allocation_exists')
        if session.agent.definition_id:
            from .agent_definitions import AgentDefinitionUnavailable, published_version_for_agent
            from .models import WorkspaceMembership
            membership = WorkspaceMembership.objects.filter(workspace=workspace, user_id=session.owner_id).first()
            if membership is None:
                raise MaintenanceError('provision_agent_authority_unavailable')
            try:
                activation = published_version_for_agent(session.agent, membership).plugin_activation
            except (AgentDefinitionUnavailable, AttributeError):
                raise MaintenanceError('provision_agent_authority_unavailable') from None
        else:
            activation = plugin_activation_for_workspace(workspace)
        request = {'kind': 'provisionEnvironment', 'environmentProfile': environment.environment_profile,
            'environmentProfileDigest': environment.environment_profile_digest, 'userId': str(session.owner_id),
            'agentId': session.agent_id, 'pluginActivation': activation}
        digest = _digest(request)
        operation = EnvironmentMaintenanceOperation.objects.create(operation_id=body['operationId'], operation_kind='provision',
            workspace=workspace, session=session, environment=environment, request=request, request_digest=digest,
            profile_digest=environment.environment_profile_digest)
        owner = 'maintenance:' + secrets.token_hex(32)
        lease = controller_lease_digest(owner)
        epoch = environment.controller_epoch + 1
        deadline = _now_ms() + LEASE_MS
        payload = {'schema': PROVISION_SCHEMA, 'operationKind': 'provision', 'operationId': operation.pk,
            'workspaceId': workspace.pk, 'sessionId': session.pk, 'environmentId': environment.pk,
            **{name: request[name] for name in ('environmentProfile', 'environmentProfileDigest', 'userId', 'agentId', 'pluginActivation')},
            'controllerOwner': {'kind': 'maintenanceOperation', 'operationId': operation.pk},
            'controllerEpoch': epoch, 'controllerLeaseDigest': lease, 'controllerPurpose': 'execute',
            'requestDigest': digest, 'expiresAtMs': deadline}
        grant = EnvironmentMaintenanceGrant.objects.create(operation=operation, previous_controller_epoch=body['expectedControllerEpoch'],
            controller_epoch=epoch, controller_lease_digest=lease, controller_purpose='execute', expires_at_ms=deadline,
            payload=payload, signature=sign_provision_authorization(payload, settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY))
        operation.current_grant = grant
        operation.save(update_fields=['current_grant'])
        environment.controller_epoch, environment.controller_run = epoch, None
        environment.controller_maintenance_operation = operation
        environment.controller_lease_owner, environment.controller_purpose = owner, 'execute'
        environment.controller_released = False
        environment.save(update_fields=['controller_epoch', 'controller_run', 'controller_maintenance_operation',
            'controller_lease_owner', 'controller_purpose', 'controller_released'])
        return _grant_result(grant)

    with plugin_lifecycle_lock():
        return _translate_contention(issue)


def provision_completion_matches(operation, grant):
    selected = grant.payload.get('environmentProfile')
    if isinstance(selected, dict) and selected.get('schema') == E2B_PROFILE_SCHEMA:
        return _provider_completion_matches(operation, grant)
    with connection.cursor() as cursor:
        cursor.execute('SELECT environment_id,session_id,workspace_id,host_id,instance_id,container_id,profile_digest,'
            'controller_epoch,controller_lease_digest,request_digest,completion_json FROM runtime.environment_provision_completions '
            'WHERE operation_id=%s FOR SHARE NOWAIT', [operation.pk])
        row = cursor.fetchone()
    if row is None:
        return None
    environment, session, workspace, host, instance, container, profile, epoch, lease, digest, encoded = row
    if ((environment, session, workspace, profile, epoch, lease, digest) != (operation.environment_id,
            operation.session_id, operation.workspace_id, operation.profile_digest,
            grant.controller_epoch, grant.controller_lease_digest, operation.request_digest)
            or not isinstance(host, str) or not host
            or not isinstance(instance, str) or not re.fullmatch(r'instance_[0-9a-f]{64}', instance)
            or not isinstance(container, str) or not re.fullmatch(r'[0-9a-f]{64}', container)):
        return False
    try:
        return json.loads(encoded) == {'kind': 'provisioned', 'instanceId': instance,
            'containerId': container, 'profileDigest': profile}
    except (ValueError, TypeError, RecursionError):
        return False


def _provider_completion_matches(operation, grant):
    """A provider completion is useful only with its exact verified origin.

    Runtime owns the insert-only preparation attestation. Its digest by itself
    is not evidence of allocation, ownership, or successful preparation.
    """
    payload = grant.payload
    try:
        selected = operation.request['environmentProfile']
        expected_request = {'kind': 'provisionEnvironment', **{name: payload[name] for name in
            ('environmentProfile', 'environmentProfileDigest', 'userId', 'agentId', 'pluginActivation')}}
        if (selected['schema'] != E2B_PROFILE_SCHEMA or selected['lifetime'] != 'retained'
                or operation.request != expected_request or _digest(expected_request) != operation.request_digest
                or payload['operationId'] != operation.pk
                or (payload['environmentId'], payload['sessionId'], payload['workspaceId'],
                    payload['environmentProfileDigest'], payload['requestDigest']) !=
                   (operation.environment_id, operation.session_id, operation.workspace_id,
                    operation.profile_digest, operation.request_digest)
                or environment_profile_digest(selected) != operation.profile_digest):
            return False
    except (KeyError, TypeError, ValueError, RecursionError):
        return False

    receipt_fields = ('creation_attempt_id', 'environment_id', 'session_id', 'workspace_id', 'instance_id',
        'sandbox_id', 'profile_digest', 'provider_config_digest', 'authorization_digest', 'controller_epoch',
        'controller_lease_digest', 'request_digest', 'preparation_digest', 'recorded_at_ms')
    creation_fields = ('environment_id', 'session_id', 'workspace_id', 'instance_id', 'verified_sandbox_id',
        'verification_digest', 'verified_at_ms', 'authorization_digest', 'profile_digest', 'provider_config_digest',
        'provider', 'dispatch_state', 'creation_owner_json', 'maintenance_operation_id', 'agent_run_id', 'processing_job_id')
    columns = [f'receipt.{name} AS receipt_{name}' for name in receipt_fields]
    columns.extend(f'creation.{name} AS creation_{name}' for name in creation_fields)
    with connection.cursor() as cursor:
        cursor.execute('SELECT ' + ','.join(columns) +
            ' FROM runtime.provider_environment_provision_completions receipt '
            'JOIN runtime.provider_creation_operations creation ON creation.attempt_id=receipt.creation_attempt_id '
            'JOIN runtime.provider_environment_bindings binding ON binding.creation_attempt_id=creation.attempt_id '
            'AND binding.environment_id=receipt.environment_id '
            'WHERE receipt.operation_id=%s FOR SHARE OF receipt,creation,binding NOWAIT', [operation.pk])
        row = cursor.fetchone()
        if row is None:
            return None
        facts = dict(zip((column[0] for column in cursor.description), row))

    expected = {'environment_id': operation.environment_id, 'session_id': operation.session_id,
        'workspace_id': operation.workspace_id, 'profile_digest': operation.profile_digest,
        'provider_config_digest': selected['providerConfigDigest'], 'authorization_digest': grant.digest}
    if any(facts['receipt_' + name] != value or facts['creation_' + name] != value
           for name, value in expected.items()):
        return False
    if ((facts['receipt_controller_epoch'], facts['receipt_controller_lease_digest'], facts['receipt_request_digest']) !=
            (grant.controller_epoch, grant.controller_lease_digest, operation.request_digest)
            or facts['creation_provider'] != 'e2b' or facts['creation_dispatch_state'] != 'dispatched'
            or facts['creation_maintenance_operation_id'] != operation.pk
            or facts['creation_agent_run_id'] is not None or facts['creation_processing_job_id'] is not None
            or facts['receipt_instance_id'] != facts['creation_instance_id']
            or not isinstance(facts['receipt_instance_id'], str)
            or not re.fullmatch(r'instance_[0-9a-f]{64}', facts['receipt_instance_id'])
            or facts['receipt_sandbox_id'] != facts['creation_verified_sandbox_id']):
        return False
    sandbox = facts['receipt_sandbox_id']
    if not isinstance(sandbox, str) or not re.fullmatch(r'[A-Za-z0-9_.:-]{1,256}', sandbox):
        return False
    for name in ('receipt_preparation_digest', 'creation_verification_digest'):
        if not isinstance(facts[name], str) or not re.fullmatch(r'sha256:[0-9a-f]{64}', facts[name]):
            return False
    for name in ('receipt_recorded_at_ms', 'creation_verified_at_ms'):
        if type(facts[name]) is not int or facts[name] < 0:
            return False
    if facts['receipt_recorded_at_ms'] < facts['creation_verified_at_ms']:
        return False
    try:
        return json.loads(facts['creation_creation_owner_json']) == {
            'kind': 'maintenanceOperation', 'operationId': operation.pk}
    except (TypeError, ValueError, RecursionError):
        return False
