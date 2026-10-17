"""Independent maintenance binding for immutable benchmark host configuration."""
import hashlib
import json
from pathlib import PurePosixPath
import re
import unicodedata

from django.db import transaction
from django.utils import timezone

PROFILE_SCHEMA = 'runtime.benchmark_execution_profile.v1'
BIND_SCHEMA = 'workspace.benchmark_profile.bind.v1'
BOUND_SCHEMA = 'workspace.benchmark_profile.bound.v1'
PROFILE_FIELDS = {'schema', 'imageDigest', 'workspaceRoot', 'user', 'network', 'resources'}
RESOURCE_FIELDS = {'cpuMilli', 'memoryBytes', 'pidsLimit', 'dataTmpfsBytes', 'storageBytes'}


class BenchmarkProfileError(ValueError):
    def __init__(self, code, status=409):
        super().__init__(code)
        self.code = code
        self.status = status


def validate_benchmark_profile(value):
    if not isinstance(value, dict) or set(value) != PROFILE_FIELDS or value['schema'] != PROFILE_SCHEMA:
        raise ValueError('benchmark_profile_schema_invalid')
    image = value['imageDigest']
    if not isinstance(image, str) or not re.fullmatch(r'sha256:[0-9a-f]{64}', image):
        raise ValueError('benchmark_profile_image_invalid')
    root = value['workspaceRoot']
    if (not isinstance(root, str) or not root.startswith('/') or root.startswith('//')
            or len(root.encode('utf-8')) > 4096 or any(unicodedata.category(c) == 'Cc' for c in root)
            or '\\' in root or unicodedata.normalize('NFC', root) != root
            or '..' in root.split('/') or str(PurePosixPath(root)) != root
            or root == '/run/centaeris' or root.startswith('/run/centaeris/')):
        raise ValueError('benchmark_profile_workspace_root_invalid')
    user = value['user']
    if not isinstance(user, str) or not re.fullmatch(r'(0|[1-9][0-9]*):(0|[1-9][0-9]*)', user):
        raise ValueError('benchmark_profile_user_invalid')
    if any(int(part) >= 2**32 - 1 for part in user.split(':')):
        raise ValueError('benchmark_profile_user_invalid')
    if value['network'] not in ('publicInternet', 'disabled'):
        raise ValueError('benchmark_profile_network_invalid')
    resources = value['resources']
    if not isinstance(resources, dict) or set(resources) != RESOURCE_FIELDS:
        raise ValueError('benchmark_profile_resources_invalid')
    for field, number in resources.items():
        maximum = 2**32 - 1 if field in ('cpuMilli', 'pidsLimit') else 2**64 - 1
        if type(number) is not int or not 0 < number <= maximum:
            raise ValueError('benchmark_profile_resources_invalid')


def benchmark_profile_digest(value):
    validate_benchmark_profile(value)
    return 'sha256:' + hashlib.sha256(json.dumps(value, ensure_ascii=False, sort_keys=True,
        separators=(',', ':')).encode('utf-8')).hexdigest()


def _receipt(environment):
    return {'schema': BOUND_SCHEMA, 'operationId': environment.benchmark_profile_operation_id,
            'sessionId': environment.session_id, 'environmentId': environment.pk,
            'profileDigest': environment.benchmark_profile_digest, 'profile': environment.benchmark_profile}


def bind_benchmark_profile(environment_id, body):
    from .models import AgentRun, Session, SessionEnvironment
    if (not isinstance(body, dict) or set(body) != {'schema', 'operationId', 'sessionId', 'profile'}
            or body['schema'] != BIND_SCHEMA):
        raise BenchmarkProfileError('benchmark_profile_request_invalid', 400)
    for field in ('operationId', 'sessionId'):
        if not isinstance(body[field], str) or not re.fullmatch(r'[A-Za-z0-9_.-]{1,128}', body[field]):
            raise BenchmarkProfileError('benchmark_profile_request_invalid', 400)
    try:
        digest = benchmark_profile_digest(body['profile'])
    except (ValueError, TypeError, OverflowError) as error:
        raise BenchmarkProfileError('benchmark_profile_request_invalid', 400) from error
    with transaction.atomic():
        # AgentRun.save() takes this same lock before allocating its environment.
        session = Session.objects.select_for_update().filter(pk=body['sessionId'], status='active').first()
        if session is None or session.current_environment_id != environment_id:
            raise BenchmarkProfileError('benchmark_profile_environment_not_found', 404)
        environment = SessionEnvironment.objects.select_for_update().get(pk=environment_id, session=session)
        if environment.benchmark_profile is not None:
            if (environment.benchmark_profile_operation_id != body['operationId']
                    or environment.benchmark_profile_digest != digest
                    or environment.benchmark_profile != body['profile']):
                raise BenchmarkProfileError('benchmark_profile_binding_immutable')
            return _receipt(environment)
        if AgentRun.objects.filter(session=session).exists() or session.events.exists():
            raise BenchmarkProfileError('benchmark_profile_requires_unused_session')
        environment.benchmark_profile = body['profile']
        environment.benchmark_profile_digest = digest
        environment.benchmark_profile_operation_id = body['operationId']
        environment.benchmark_profile_bound_at = timezone.now()
        environment.save(update_fields=['benchmark_profile', 'benchmark_profile_digest',
            'benchmark_profile_operation_id', 'benchmark_profile_bound_at'])
        return _receipt(environment)


def complete_benchmark_setup(environment_id, body):
    from .models import AgentRun, Session, SessionEnvironment
    from .runtime_contract import authorization_digest, agent_run_binding_matches, verify_agent_run_authorization_signature
    from django.conf import settings
    fields = {'schema', 'operationId', 'sessionId', 'agentRunId', 'instanceId', 'containerId'}
    if (not isinstance(body, dict) or set(body) != fields
            or body['schema'] != 'workspace.benchmark_setup.complete.v1'):
        raise BenchmarkProfileError('benchmark_setup_request_invalid', 400)
    for field in ('operationId', 'sessionId', 'agentRunId'):
        if not isinstance(body[field], str) or not re.fullmatch(r'[A-Za-z0-9_.-]{1,128}', body[field]):
            raise BenchmarkProfileError('benchmark_setup_request_invalid', 400)
    if (not isinstance(body['instanceId'], str) or not re.fullmatch(r'instance_[0-9a-f]{64}', body['instanceId'])
            or not isinstance(body['containerId'], str) or not re.fullmatch(r'[0-9a-f]{64}', body['containerId'])):
        raise BenchmarkProfileError('benchmark_setup_request_invalid', 400)
    with transaction.atomic():
        session = Session.objects.select_for_update().filter(pk=body['sessionId'], status='active').first()
        if session is None or session.current_environment_id != environment_id:
            raise BenchmarkProfileError('benchmark_profile_environment_not_found', 404)
        environment = SessionEnvironment.objects.select_for_update().get(pk=environment_id, session=session)
        if environment.benchmark_setup_operation_id:
            if (environment.benchmark_setup_operation_id != body['operationId']
                    or environment.benchmark_setup_run_id != body['agentRunId']
                    or environment.benchmark_setup_instance_id != body['instanceId']
                    or environment.benchmark_setup_container_id != body['containerId']):
                raise BenchmarkProfileError('benchmark_setup_binding_immutable')
        else:
            run = AgentRun.objects.select_related('authorization', 'environment').filter(
                pk=body['agentRunId'], session=session, environment=environment).first()
            if (environment.benchmark_profile is None or run is None
                    or run.status not in {'queued', 'running'}
                    or AgentRun.objects.filter(session=session).exclude(pk=run.pk).exists()):
                raise BenchmarkProfileError('benchmark_setup_run_not_admissible')
            try:
                grant = run.authorization
                if (grant.payload['schema'] != 'workspace.agent_run_authorization.v2'
                        or authorization_digest(grant.payload) != grant.digest
                        or not agent_run_binding_matches(grant.payload, run)):
                    raise ValueError('binding')
                verify_agent_run_authorization_signature(grant.payload,
                    settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY, grant.signature)
            except (ValueError, AttributeError, KeyError, AgentRun.authorization.RelatedObjectDoesNotExist) as error:
                raise BenchmarkProfileError('benchmark_setup_authorization_invalid') from error
            environment.benchmark_setup_operation_id = body['operationId']
            environment.benchmark_setup_run = run
            environment.benchmark_setup_instance_id = body['instanceId']
            environment.benchmark_setup_container_id = body['containerId']
            environment.benchmark_setup_completed_at = timezone.now()
            environment.save(update_fields=['benchmark_setup_operation_id', 'benchmark_setup_run',
                'benchmark_setup_instance_id', 'benchmark_setup_container_id', 'benchmark_setup_completed_at'])
        return {'schema': 'workspace.benchmark_setup.completed.v1',
                'operationId': environment.benchmark_setup_operation_id, 'sessionId': session.pk,
                'environmentId': environment.pk, 'agentRunId': environment.benchmark_setup_run_id,
                'instanceId': environment.benchmark_setup_instance_id,
                'containerId': environment.benchmark_setup_container_id}
