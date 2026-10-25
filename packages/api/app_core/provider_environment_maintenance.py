"""Provider allocation facts for independent maintenance, without native aliases."""
import json

from django.conf import settings
from django.db import connection

from .environment_profile import E2B_PROFILE_SCHEMA, environment_profile_digest
from .models import AgentRunAuthorization, AgentRunEnvironmentRelease, EnvironmentMaintenanceOperation
from .provider_environment_maintenance_authorization import validate_provider_target


def provider_allocation(workspace, session, environment, *, check_availability=True):
    from .environment_maintenance import MaintenanceError
    selected = environment.environment_profile
    if (not isinstance(selected, dict) or selected.get('schema') != E2B_PROFILE_SCHEMA
            or selected.get('lifetime') != 'retained'
            or environment_profile_digest(selected) != environment.environment_profile_digest):
        raise MaintenanceError('maintenance_allocation_profile_mismatch')
    fields = ('attempt_id', 'instance_id', 'environment_id', 'session_id', 'workspace_id', 'profile_digest',
        'provider_config_digest', 'provider', 'dispatch_state', 'verified_sandbox_id', 'verification_digest',
        'verified_at_ms', 'authorization_digest', 'creation_owner_json', 'maintenance_operation_id',
        'agent_run_id', 'processing_job_id', 'settlement_id')
    with connection.cursor() as cursor:
        cursor.execute('SELECT ' + ','.join('creation.' + field for field in fields) +
            ' FROM runtime.provider_environment_bindings binding JOIN runtime.provider_creation_operations creation '
            'ON creation.attempt_id=binding.creation_attempt_id AND creation.environment_id=binding.environment_id '
            'WHERE binding.environment_id=%s LIMIT 2 FOR SHARE OF binding,creation NOWAIT', [environment.pk])
        rows = cursor.fetchall()
        if len(rows) != 1:
            raise MaintenanceError('maintenance_allocation_unavailable')
        facts = dict(zip(fields, rows[0]))
        if check_availability:
            cursor.execute('SELECT 1 FROM runtime.provider_environment_retirements WHERE environment_id=%s FOR SHARE NOWAIT',
                [environment.pk])
            if cursor.fetchone() is not None:
                raise MaintenanceError('maintenance_environment_retiring')
            # Lock source facts, since the admission projection uses DISTINCT.
            cursor.execute('SELECT 1 FROM runtime.provider_environment_power_operations '
                'WHERE environment_id=%s FOR SHARE NOWAIT', [environment.pk])
            cursor.fetchall()
            cursor.execute('SELECT blocked FROM runtime.provider_environment_power_admission WHERE environment_id=%s',
                [environment.pk])
            power = cursor.fetchone()
            if power is not None and power[0] is not False:
                raise MaintenanceError('maintenance_environment_power_unavailable')
    if ((facts['environment_id'], facts['session_id'], facts['workspace_id'], facts['profile_digest'],
            facts['provider_config_digest']) !=
            (environment.pk, session.pk, workspace.pk, environment.environment_profile_digest, selected['providerConfigDigest'])
            or facts['provider'] != 'e2b' or facts['dispatch_state'] != 'dispatched'
            or facts['settlement_id'] is not None or type(facts['verified_at_ms']) is not int or facts['verified_at_ms'] < 0):
        raise MaintenanceError('maintenance_allocation_unavailable')
    target = {'provider': 'e2b', 'creationAttemptId': facts['attempt_id'], 'instanceId': facts['instance_id'],
        'sandboxId': facts['verified_sandbox_id'], 'verificationDigest': facts['verification_digest'],
        'providerConfigDigest': facts['provider_config_digest']}
    try:
        validate_provider_target(target)
        owner = json.loads(facts['creation_owner_json'])
    except (TypeError, ValueError, RecursionError):
        raise MaintenanceError('maintenance_allocation_unavailable') from None
    if not _released_origin(facts, owner, workspace, session, environment):
        raise MaintenanceError('maintenance_allocation_unavailable')
    return dict(provider_target=target, instance_id=None, container_id=None, host_id=None,
        allocation_execution_id=None, profile_digest=environment.environment_profile_digest), facts['verified_at_ms']


def _released_origin(facts, owner, workspace, session, environment):
    from .environment_maintenance import _grant_result, MaintenanceError
    if (isinstance(owner, dict) and set(owner) == {'kind', 'operationId'} and owner['kind'] == 'maintenanceOperation'
            and owner['operationId'] == facts['maintenance_operation_id']
            and facts['agent_run_id'] is None and facts['processing_job_id'] is None):
        original = EnvironmentMaintenanceOperation.objects.select_for_update().filter(pk=owner['operationId'],
            operation_kind='provision', environment=environment, session=session, workspace=workspace, state='released').first()
        if original is None or original.current_grant_id is None:
            return False
        grant = original.current_grant
        if grant.digest != facts['authorization_digest']:
            return False
        try:
            _grant_result(grant)
        except MaintenanceError:
            return False
        from .environment_provision import provision_completion_matches
        return provision_completion_matches(original, grant) is True
    if (not isinstance(owner, dict) or set(owner) != {'kind', 'runId'} or owner['kind'] != 'agentRun'
            or owner['runId'] != facts['agent_run_id'] or facts['maintenance_operation_id'] is not None
            or not isinstance(facts['processing_job_id'], str) or not facts['processing_job_id']):
        return False
    grant = AgentRunAuthorization.objects.select_related('agent_run').filter(
        agent_run_id=owner['runId'], digest=facts['authorization_digest']).first()
    if grant is None:
        return False
    from .runtime_contract import authorization_digest, validate_agent_run_authorization_payload, verify_agent_run_authorization_signature
    try:
        validate_agent_run_authorization_payload(grant.payload)
        verify_agent_run_authorization_signature(grant.payload, settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY, grant.signature)
        valid = (authorization_digest(grant.payload) == grant.digest
            and (grant.agent_run.environment_id, grant.agent_run.session_id, grant.agent_run.workspace_id) ==
                (environment.pk, session.pk, workspace.pk)
            and (grant.payload['agentRunId'], grant.payload['sessionId'], grant.payload['workspaceId'],
                 grant.payload['environmentProfile'], grant.payload['environmentProfileDigest']) ==
                (owner['runId'], session.pk, workspace.pk, environment.environment_profile, environment.environment_profile_digest))
    except (KeyError, ValueError, TypeError, RecursionError):
        return False
    return valid and AgentRunEnvironmentRelease.objects.filter(agent_run_id=owner['runId'], environment=environment,
        authorization_digest=grant.digest).exists()


def provider_completion_matches(operation, grant):
    from .environment_maintenance import MaintenanceError, _valid_task_completion
    fields = ('environment_id', 'creation_attempt_id', 'instance_id', 'sandbox_id', 'profile_digest',
        'provider_config_digest', 'verification_digest', 'controller_epoch', 'controller_lease_digest',
        'request_digest', 'completion_json', 'recorded_at_ms')
    with connection.cursor() as cursor:
        cursor.execute('SELECT ' + ','.join(fields) + ' FROM runtime.provider_environment_maintenance_completions '
            'WHERE operation_id=%s FOR SHARE NOWAIT', [operation.pk])
        row = cursor.fetchone()
    if row is None:
        return _direct_event_completion_matches(operation, grant)
    facts = dict(zip(fields, row))
    try:
        validate_provider_target(operation.provider_target)
        target = operation.provider_target
        expected = {'environment_id': operation.environment_id, 'creation_attempt_id': target['creationAttemptId'],
            'instance_id': target['instanceId'], 'sandbox_id': target['sandboxId'], 'profile_digest': operation.profile_digest,
            'provider_config_digest': target['providerConfigDigest'], 'verification_digest': target['verificationDigest'],
            'controller_epoch': grant.controller_epoch, 'controller_lease_digest': grant.controller_lease_digest,
            'request_digest': operation.request_digest}
        if any(facts[name] != value for name, value in expected.items()):
            return False
        selected, verified_at = provider_allocation(operation.workspace, operation.session, operation.environment,
            check_availability=False)
        if any(getattr(operation, name) != value for name, value in selected.items()):
            return False
        if type(facts['recorded_at_ms']) is not int or facts['recorded_at_ms'] < verified_at:
            return False
        # The existing resident task protocol remains an independent contract.
        return _valid_task_completion(facts['completion_json'])
    except (KeyError, ValueError, TypeError, RecursionError, MaintenanceError):
        return False



def _direct_event_completion_matches(operation, grant):
    from .environment_maintenance import MaintenanceError
    try:
        selected, verified_at = provider_allocation(operation.workspace, operation.session, operation.environment,
            check_availability=False)
        if any(getattr(operation, name) != value for name, value in selected.items()):
            return False
        return _checked_direct_events(operation, grant, verified_at)
    except (KeyError, ValueError, TypeError, RecursionError, MaintenanceError):
        return False


def _checked_direct_events(operation, grant, verified_at):
    from pathlib import PurePosixPath
    from .provider_environment_maintenance_completion import (
        validate_direct_completion, dispatch_record, SCHEMA as COMPLETED_SCHEMA, DISPATCH_SCHEMA)
    identifiers = ['provider_maintenance_dispatched:' + operation.pk, 'provider_maintenance_completed:' + operation.pk]
    with connection.cursor() as cursor:
        cursor.execute('SELECT event_id,session_id,task_id,event_type,at_ms,visibility,payload_json '
            'FROM runtime.runtime_events WHERE event_id IN (%s,%s) FOR SHARE NOWAIT', identifiers)
        events = {row[0]: row[1:] for row in cursor.fetchall()}
    if not events or set(events) == {identifiers[0]}:
        return None
    if set(events) != set(identifiers):
        return False
    completion = json.loads(events[identifiers[1]][-1])
    expected = {key: grant.payload[key] for key in ('environmentId', 'providerTarget', 'profileDigest',
        'requestDigest', 'controllerEpoch', 'controllerLeaseDigest')}
    expected.update(operationId=operation.pk, authorizationDigest=grant.digest)
    validate_direct_completion(completion, expected)
    request, output = operation.request, completion['output']
    if set(request) == {'program', 'args', 'cwd', 'timeoutMs'}:
        if output['kind'] != 'command':
            return False
    elif set(request) == {'request', 'contentLength', 'contentDigest'}:
        selected = request['request']
        if not isinstance(selected, dict):
            return False
        kind = selected.get('kind')
        if output['kind'] == 'filesystemFailed':
            # Only a confirmed read failure is terminal without a mutation outcome.
            if kind not in {'readFile', 'listDirectory'}:
                return False
        elif kind == 'writeFile':
            if (output['kind'] != 'fileWritten' or output['byteLength'] != request['contentLength']
                    or output['sha256'] != request['contentDigest']):
                return False
        elif kind == 'readFile':
            if output['kind'] != 'fileRead' or output['byteLength'] > selected['maxBytes']:
                return False
        elif kind == 'listDirectory':
            root = PurePosixPath(selected['path'])
            if (output['kind'] != 'directoryListed' or len(output['entries']) > selected['maxEntries']
                    or any(not PurePosixPath(entry['path']).is_relative_to(root) or PurePosixPath(entry['path']) == root
                        for entry in output['entries'])):
                return False
        else:
            return False
    else:
        return False
    previous = verified_at
    for identifier, schema, payload in zip(identifiers, (DISPATCH_SCHEMA, COMPLETED_SCHEMA),
            (dispatch_record(completion), completion)):
        session, task, event_type, at_ms, visibility, encoded = events[identifier]
        if (session != operation.session_id or task is not None or event_type != schema or visibility != 'internal'
                or type(at_ms) is not int or at_ms < previous
                or json.loads(encoded) != payload):
            return False
        previous = at_ms
    return True
