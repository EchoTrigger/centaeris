"""Run through manage.py shell only in the newly created fixture database.

Creates synthetic business inputs, never execution/terminal/receipt records.
Private generated credentials remain inside the fixture's storage volume.
"""
import json
import os
from pathlib import Path
import secrets

from django.conf import settings
from django.contrib.auth import get_user_model
from django.db import connection, transaction
from app_core.credentials import encrypt_credential_secret
from app_core.models import (Agent, ModelConfig, ModelProvider, ProviderCredential,
                             Workspace, WorkspaceMembership)

assert settings.DATABASES['default']['NAME'] == 'retained_acceptance'
execution_mode=os.environ.get('AGENT_RUN_EXECUTION_MODE')
assert execution_mode in ('perRun','retained'), 'explicit supported execution mode required'
assert not Workspace.objects.exists(), 'refusing to seed an existing workspace database'
output = Path(settings.MEDIA_ROOT) / 'acceptance-seed.json'
assert not output.exists(), 'refusing to replace fixture credentials'
shared = os.environ.get('CENTAERIS_ACCEPTANCE_SHARED_WORKLOAD') == '1'
assert shared or execution_mode=='retained', 'rotation requires retained execution mode'
email = 'retained-' + secrets.token_hex(8) + '@fixture.invalid'
password = secrets.token_urlsafe(40)
with transaction.atomic():
    user = get_user_model().objects.create_user(username=email, email=email, password=password)
    workspace = Workspace.objects.create(name='Retained isolated acceptance', createdBy=user)
    WorkspaceMembership.objects.create(workspace=workspace, user=user, role='owner')
    provider = ModelProvider.objects.create(workspace=workspace, displayName='Owned deterministic fixture',
        api=('openai-responses' if shared else 'openai-completions'), apiBase=('https://model:9999/v1' if shared else 'https://model:8080/v1'))
    ProviderCredential.objects.create(provider=provider, displayName='Synthetic fixture only',
        encryptedSecret=encrypt_credential_secret(secrets.token_urlsafe(40)),
        createdBy=user, updatedBy=user)
    model = ModelConfig.objects.create(provider=provider, displayName='Deterministic acceptance',
        modelName=('perf-fixed-workload' if shared else 'retained-acceptance'), contextTokens=131072, maxOutputTokens=4096)
    agent = Agent.objects.create(workspace=workspace, owner=user, name='Acceptance task runner',
        instructions='Follow the acceptance prompt and invoke only the requested tools.',
        model_config=model)
output.parent.mkdir(parents=True, exist_ok=True)
fd = os.open(output, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
with os.fdopen(fd, 'w') as stream:
    json.dump({'email':email,'password':password,'workspaceId':workspace.id,
               'agentId':agent.id,'modelConfigRef':model.id,'executionMode':execution_mode}, stream)
print(json.dumps({'status':'synthetic-business-inputs-created','workspaceId':workspace.id,
                  'agentId':agent.id,'modelConfigRef':model.id,'executionMode':execution_mode}))
