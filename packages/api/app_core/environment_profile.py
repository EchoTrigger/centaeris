"""Immutable environment execution configuration, shared by every caller."""
import copy
import hashlib
import json
from pathlib import PurePosixPath
import re
import unicodedata
from uuid import UUID

from django.db import transaction
from django.utils import timezone

PROFILE_SCHEMA = 'runtime.environment_profile.v1'
E2B_PROFILE_SCHEMA = 'runtime.e2b_environment_profile.v1'
BIND_SCHEMA = 'workspace.environment_profile.bind.v1'
BOUND_SCHEMA = 'workspace.environment_profile.bound.v1'
PROFILE_FIELDS = {'schema', 'imageDigest', 'workspaceRoot', 'user', 'network', 'resources'}
RESOURCE_FIELDS = {'cpuMilli', 'memoryBytes', 'pidsLimit', 'dataTmpfsBytes', 'storageBytes'}
E2B_PROFILE_FIELDS = {'schema', 'provider', 'providerConfigDigest', 'templateRef', 'workspaceRoot',
    'user', 'network', 'lifetime', 'timeoutSeconds', 'inputBudgetBytes'}
MAX_EXECUTION_INPUT_BYTES = 64 * 1024 * 1024


class EnvironmentProfileError(ValueError):
    def __init__(self, code, status=409):
        super().__init__(code)
        self.code = code
        self.status = status


def validate_environment_profile(value):
    if isinstance(value, dict) and value.get('schema') == E2B_PROFILE_SCHEMA:
        _validate_e2b_profile(value)
        return
    if not isinstance(value, dict) or set(value) != PROFILE_FIELDS or value['schema'] != PROFILE_SCHEMA:
        raise ValueError('environment_profile_schema_invalid')
    image = value['imageDigest']
    if not isinstance(image, str) or not re.fullmatch(r'sha256:[0-9a-f]{64}', image):
        raise ValueError('environment_profile_image_invalid')
    _validate_workspace_root(value['workspaceRoot'])
    user = value['user']
    if not isinstance(user, str) or not re.fullmatch(r'(0|[1-9][0-9]*):(0|[1-9][0-9]*)', user):
        raise ValueError('environment_profile_user_invalid')
    if any(int(part) >= 2**32 - 1 for part in user.split(':')):
        raise ValueError('environment_profile_user_invalid')
    if value['network'] not in ('publicInternet', 'disabled'):
        raise ValueError('environment_profile_network_invalid')
    resources = value['resources']
    if not isinstance(resources, dict) or set(resources) != RESOURCE_FIELDS:
        raise ValueError('environment profile resources fields mismatch')
    for field, number in resources.items():
        maximum = 2**32 - 1 if field in ('cpuMilli', 'pidsLimit') else 2**64 - 1
        if type(number) is not int or number <= 0:
            raise ValueError('environment profile resources must be positive integers')
        if number > maximum:
            raise ValueError(f'environment profile resources.{field} exceeds transport range')


def _validate_workspace_root(root):
    if (not isinstance(root, str) or not root.startswith('/') or root.startswith('//')
            or len(root.encode('utf-8')) > 4096 or any(unicodedata.category(c) == 'Cc' for c in root)
            or '\\' in root or unicodedata.normalize('NFC', root) != root
            or '..' in root.split('/') or str(PurePosixPath(root)) != root
            or root == '/run/centaeris' or root.startswith('/run/centaeris/')):
        raise ValueError('environment_profile_workspace_root_invalid')


def _validate_e2b_profile(value):
    if set(value) != E2B_PROFILE_FIELDS or value['provider'] != 'e2b':
        raise ValueError('environment_profile_schema_invalid')
    digest = value['providerConfigDigest']
    if not isinstance(digest, str) or not re.fullmatch(r'sha256:[0-9a-f]{64}', digest):
        raise ValueError('environment_profile_provider_config_invalid')
    template = value['templateRef']
    if (not isinstance(template, dict) or set(template) != {'templateId', 'buildId'}
            or not isinstance(template['templateId'], str)
            or not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_-]{0,127}', template['templateId'])):
        raise ValueError('environment_profile_template_invalid')
    build = template['buildId']
    try:
        if not isinstance(build, str) or str(UUID(build)) != build:
            raise ValueError('environment_profile_template_invalid')
    except (ValueError, AttributeError) as error:
        raise ValueError('environment_profile_template_invalid') from error
    _validate_workspace_root(value['workspaceRoot'])
    if not isinstance(value['user'], str) or not re.fullmatch(r'[a-z_][a-z0-9_-]{0,31}', value['user']):
        raise ValueError('environment_profile_user_invalid')
    if value['network'] not in ('publicInternet', 'disabled'):
        raise ValueError('environment_profile_network_invalid')
    if value['lifetime'] not in ('perRun', 'retained'):
        raise ValueError('environment_profile_lifetime_invalid')
    if type(value['timeoutSeconds']) is not int or not 1 <= value['timeoutSeconds'] <= 86400:
        raise ValueError('environment_profile_timeout_invalid')
    if (type(value['inputBudgetBytes']) is not int
            or not 1 <= value['inputBudgetBytes'] <= MAX_EXECUTION_INPUT_BYTES):
        raise ValueError('environment_profile_input_budget_invalid')


def validate_execution_profile_advertisement(value):
    if isinstance(value, dict) and value.get('schema') == 'runtime.execution_profile.v2':
        if set(value) != {'schema', 'environmentProfile'}:
            raise ValueError('runtime_execution_profile_response_invalid')
        validate_environment_profile(value['environmentProfile'])
        if value['environmentProfile']['schema'] != E2B_PROFILE_SCHEMA:
            raise ValueError('runtime_execution_profile_response_invalid')
        return
    if (not isinstance(value, dict) or set(value) != {'schema', 'imageDigest'}
            or value['schema'] != 'runtime.execution_profile.v1'
            or not isinstance(value['imageDigest'], str)
            or not re.fullmatch(r'sha256:[0-9a-f]{64}', value['imageDigest'])):
        raise ValueError('runtime_execution_profile_response_invalid')


def environment_input_budget_bytes(value):
    """Transfer admission is independent of provider filesystem quota guarantees."""
    validate_environment_profile(value)
    if value['schema'] == E2B_PROFILE_SCHEMA:
        return value['inputBudgetBytes']
    return value['resources']['dataTmpfsBytes'] // 2


def environment_workspace_transfer_limit_bytes(value):
    """Bound workspace upload/restore traffic, without implying a provider disk quota."""
    validate_environment_profile(value)
    if value['schema'] == E2B_PROFILE_SCHEMA:
        return value['inputBudgetBytes']
    return value['resources']['dataTmpfsBytes']


def validate_environment_lifetime(value, execution_mode):
    if isinstance(value, dict) and value.get('schema') == E2B_PROFILE_SCHEMA:
        validate_environment_profile(value)
        if value['lifetime'] != execution_mode:
            raise ValueError('environment_lifetime_mismatch')


def environment_profile_digest(value):
    validate_environment_profile(value)
    return 'sha256:' + hashlib.sha256(json.dumps(value, ensure_ascii=False, sort_keys=True,
        separators=(',', ':')).encode('utf-8')).hexdigest()


def _receipt(environment):
    return {'schema': BOUND_SCHEMA, 'operationId': environment.environment_profile_operation_id,
            'sessionId': environment.session_id, 'environmentId': environment.pk,
            'profileDigest': environment.environment_profile_digest, 'profile': environment.environment_profile}


def freeze_run_environment_profile(agent_run, image_digest=None, *, execution_profile=None):
    """First use freezes a new environment; history is never guessed from defaults."""
    from django.conf import settings
    from .models import AgentRun, Session, SessionEnvironment
    if execution_profile is not None:
        if image_digest is not None:
            raise ValueError('environment_profile_sources_conflict')
        validate_execution_profile_advertisement(execution_profile)
        image_digest = execution_profile.get('imageDigest')
    with transaction.atomic():
        session = Session.objects.select_for_update().get(pk=agent_run.session_id)
        environment = SessionEnvironment.objects.select_for_update().get(pk=agent_run.environment_id)
        if session.current_environment_id != environment.pk or environment.session_id != session.pk:
            raise ValueError('environment_profile_binding_invalid')
        if environment.environment_profile is None:
            if AgentRun.objects.filter(environment=environment).exclude(pk=agent_run.pk).exists():
                raise ValueError('environment_profile_history_unavailable')
            if hasattr(agent_run, 'authorization'):
                raise ValueError('environment_profile_history_unavailable')
            if execution_profile is not None and execution_profile['schema'] == 'runtime.execution_profile.v2':
                selected = copy.deepcopy(execution_profile['environmentProfile'])
                if selected['network'] != environment.network:
                    raise ValueError('environment_profile_network_mismatch')
            else:
                selected = {
                    'schema': PROFILE_SCHEMA, 'imageDigest': image_digest, 'workspaceRoot': '/mnt/data',
                    'user': '10001:10001', 'network': environment.network,
                    'resources': {'memoryBytes': settings.SANDBOX_MEMORY_BYTES,
                        'cpuMilli': settings.SANDBOX_CPU_MILLI, 'pidsLimit': settings.SANDBOX_PIDS_LIMIT,
                        'dataTmpfsBytes': settings.SANDBOX_DATA_TMPFS_BYTES,
                        'storageBytes': settings.SANDBOX_DATA_TMPFS_BYTES}}
            environment.environment_profile = selected
            environment.environment_profile_digest = environment_profile_digest(environment.environment_profile)
            environment.environment_profile_operation_id = f'first_run:{agent_run.pk}'
            environment.environment_profile_bound_at = timezone.now()
            environment.save(update_fields=['environment_profile', 'environment_profile_digest',
                'environment_profile_operation_id', 'environment_profile_bound_at'])
        if environment_profile_digest(environment.environment_profile) != environment.environment_profile_digest:
            raise ValueError('environment_profile_digest_invalid')
        agent_run.environment = environment
        if agent_run.session.current_environment_id == environment.pk:
            agent_run.session.current_environment = environment
        return environment


def bind_environment_profile(environment_id, body):
    from .models import AgentRun, Session, SessionEnvironment
    if (not isinstance(body, dict) or set(body) != {'schema', 'operationId', 'sessionId', 'profile'}
            or body['schema'] != BIND_SCHEMA):
        raise EnvironmentProfileError('environment_profile_request_invalid', 400)
    for field in ('operationId', 'sessionId'):
        if not isinstance(body[field], str) or not re.fullmatch(r'[A-Za-z0-9_.-]{1,128}', body[field]):
            raise EnvironmentProfileError('environment_profile_request_invalid', 400)
    try:
        digest = environment_profile_digest(body['profile'])
    except (ValueError, TypeError, OverflowError) as error:
        raise EnvironmentProfileError('environment_profile_request_invalid', 400) from error
    with transaction.atomic():
        # AgentRun.save() takes this same lock before allocating its environment.
        session = Session.objects.select_for_update().filter(pk=body['sessionId'], status='active').first()
        if session is None or session.current_environment_id != environment_id:
            raise EnvironmentProfileError('environment_profile_environment_not_found', 404)
        environment = SessionEnvironment.objects.select_for_update().get(pk=environment_id, session=session)
        if environment.environment_profile is not None:
            if (environment.environment_profile_operation_id != body['operationId']
                    or environment.environment_profile_digest != digest
                    or environment.environment_profile != body['profile']):
                raise EnvironmentProfileError('environment_profile_binding_immutable')
            return _receipt(environment)
        if AgentRun.objects.filter(session=session).exists() or session.events.exists():
            raise EnvironmentProfileError('environment_profile_requires_unused_session')
        environment.environment_profile = body['profile']
        environment.network = body['profile']['network']
        environment.environment_profile_digest = digest
        environment.environment_profile_operation_id = body['operationId']
        environment.environment_profile_bound_at = timezone.now()
        environment.save(update_fields=['network', 'environment_profile', 'environment_profile_digest',
            'environment_profile_operation_id', 'environment_profile_bound_at'])
        return _receipt(environment)
