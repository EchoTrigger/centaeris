"""One-shot real-provider business setup inside a fresh owned benchmark database.

Run under manage.py shell with this module's main() and JSON stdin. Credentials
are not command arguments or persistent container environment. This script never
creates Sessions, Runs, execution records, receipts, or model requests.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
import re
import secrets
import sys

INPUT_SCHEMA = 'centaeris.workspaceBenchmarkSeedInput.v1'
FIXTURE_SCHEMA = 'centaeris.workspaceBenchmarkFixture.v1'
INPUT_LIMIT = 16384


def _unique(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError('benchmark_seed_duplicate_field')
        result[key] = value
    return result


def _decode_input(stream):
    wire = stream.read(INPUT_LIMIT + 1)
    if len(wire) > INPUT_LIMIT:
        raise ValueError('benchmark_seed_input_too_large')
    try:
        value = json.loads(wire, object_pairs_hook=_unique)
    except (ValueError, TypeError) as error:
        raise ValueError('benchmark_seed_input_invalid') from None
    if (not isinstance(value, dict) or set(value) != {'schema', 'credential', 'model'}
            or value['schema'] != INPUT_SCHEMA or not isinstance(value['credential'], str)
            or not value['credential'] or value['credential'] != value['credential'].strip()
            or any(ord(c) < 33 or ord(c) == 127 for c in value['credential'])):
        raise ValueError('benchmark_seed_input_invalid')
    from workspace_plan import validate_model
    validate_model(value['model'], runnable=True)
    return value['credential'], value['model']


def seed(identity_path, input_stream):
    from django.conf import settings
    from django.contrib.auth import get_user_model
    from django.db import connection, transaction
    from app_core.credentials import encrypt_credential_secret
    from app_core.models import (Agent, AgentRun, ModelConfig, ModelProvider, ProviderCredential,
                                 Session, Workspace, WorkspaceMembership)

    identity_path = Path(identity_path)
    if identity_path.is_symlink() or not identity_path.is_file() or identity_path.stat().st_size > 16384:
        raise ValueError('benchmark_seed_fixture_invalid')
    identity = json.loads(identity_path.read_text(encoding='utf-8'), object_pairs_hook=_unique)
    fields = {'schema', 'fixtureId', 'databaseName', 'databaseUser', 'sourceRevision', 'sourceTree',
              'dockerHostId', 'dockerSocket'}
    if (not isinstance(identity, dict) or set(identity) != fields or identity['schema'] != FIXTURE_SCHEMA
            or not re.fullmatch(r'tbworkspace[0-9a-f]{10}', str(identity['fixtureId']))
            or identity['databaseName'] != identity['fixtureId'] or identity['databaseUser'] != identity['fixtureId']
            or not re.fullmatch(r'[0-9a-f]{40}', str(identity['sourceRevision']))
            or not re.fullmatch(r'[0-9a-f]{40}', str(identity['sourceTree']))
            or os.environ.get('CENTAERIS_BENCHMARK_FIXTURE_ID') != identity['fixtureId']
            or os.environ.get('AGENT_RUN_EXECUTION_MODE') != 'retained'):
        raise ValueError('benchmark_seed_fixture_invalid')
    configured = settings.DATABASES['default']
    if (connection.vendor != 'postgresql' or configured['NAME'] != identity['databaseName']
            or configured['USER'] != identity['databaseUser']):
        raise ValueError('benchmark_seed_database_not_owned')
    with connection.cursor() as cursor:
        cursor.execute('SELECT current_database(), current_user')
        if cursor.fetchone() != (identity['databaseName'], identity['databaseUser']):
            raise ValueError('benchmark_seed_database_not_owned')
        cursor.execute("SELECT EXISTS(SELECT 1 FROM django_migrations WHERE app=%s AND name=%s)",
                       ['app_core', '0020_benchmark_execution_profile'])
        if cursor.fetchone() != (True,):
            raise ValueError('benchmark_seed_migration_missing')
    output = Path(settings.MEDIA_ROOT) / 'benchmark-seed.json'
    if output.exists() or output.is_symlink():
        raise ValueError('benchmark_seed_existing_private_seed')
    secret, model_descriptor = _decode_input(input_stream)
    from workspace_plan import canonical_digest
    user_model = get_user_model()
    email = 'benchmark-' + secrets.token_hex(12) + '@fixture.invalid'
    password = secrets.token_urlsafe(40)
    with transaction.atomic():
        if any(model.objects.exists() for model in (user_model, Workspace, Agent, ModelProvider,
                ModelConfig, ProviderCredential, Session, AgentRun)):
            raise ValueError('benchmark_seed_database_not_empty')
        user = user_model.objects.create_user(username=email, email=email, password=password)
        workspace = Workspace.objects.create(name='Workspace benchmark', createdBy=user)
        WorkspaceMembership.objects.create(workspace=workspace, user=user, role='owner')
        provider = ModelProvider.objects.create(displayName='Benchmark model provider',
            api='openai-completions', apiBase=model_descriptor['apiBase'])
        ProviderCredential.objects.create(provider=provider, displayName='Controller supplied benchmark credential',
            encryptedSecret=encrypt_credential_secret(secret), createdBy=user, updatedBy=user)
        model = ModelConfig.objects.create(provider=provider, displayName='Benchmark model',
            modelName=model_descriptor['id'], contextTokens=model_descriptor['contextTokens'],
            maxOutputTokens=model_descriptor['outputTokens'], thinkingMode=model_descriptor['thinking'] or '',
            thinkingModes=[model_descriptor['thinking']] if model_descriptor['thinking'] is not None else [])
        agent = Agent.objects.create(workspace=workspace, owner=user, name='Terminal benchmark',
            instructions='', model_config=model)
        output.parent.mkdir(parents=True, exist_ok=True)
        descriptor = os.open(output, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        # Publish credentials before the DB commit. If commit acknowledgement is
        # lost, retain this file and reconcile/login; never blindly seed again.
        with os.fdopen(descriptor, 'w', encoding='utf-8') as handle:
            json.dump({'schema': 'centaeris.workspaceBenchmarkSeed.v1', 'fixtureId': identity['fixtureId'],
                'sourceRevision': identity['sourceRevision'], 'email': email, 'password': password,
                'workspaceId': workspace.id, 'agentId': agent.id, 'modelConfigRef': model.id,
                'model': model_descriptor, 'modelSha256': canonical_digest(model_descriptor)}, handle)
            handle.flush()
            os.fsync(handle.fileno())
    print(json.dumps({'status': 'benchmark-business-inputs-created', 'fixtureId': identity['fixtureId'],
        'workspaceId': workspace.id, 'agentId': agent.id, 'modelConfigRef': model.id}))


def main():
    # prepare mounts the shared descriptor validator next to this one-shot script.
    sys.path.insert(0, '/harness')
    try:
        seed(Path('/harness/benchmark-fixture.json'), sys.stdin)
    except Exception:
        # No input, credentials, database exception parameters, or private paths
        # belong in the shell's exception output. Preserve private artifacts.
        raise SystemExit('benchmark_seed_failed_preserve_private_state') from None


if __name__ == '__main__':
    main()
