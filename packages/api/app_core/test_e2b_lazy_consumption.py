"""Opt-in Linux API/Worker/main coverage with a launcher that cannot start SDK.

The unknown creation in the second case is synthetic, dispatched through the
real creation driver into this test's isolated namespace. It is never settled.
"""
from collections import deque
from copy import deepcopy
import json
import os
from pathlib import Path
import queue
import subprocess
import sys
import threading
from unittest.mock import patch
from urllib.parse import quote

from django.conf import settings
from django.contrib.auth import get_user_model
from django.db import connection
from django.test import LiveServerTestCase, override_settings

from .model_adapter.common import zero_usage
from .models import (Agent, AgentRun, AgentRunEnvironmentRelease, ModelConfig,
                     Session, Workspace, WorkspaceMembership)
from .runtime_schema_test_helpers import isolate_runtime_schema
from .test_e2b_consumption_production import drive_lifecycle_job
from . import test_native_consumption_production as native_fixture


RUNTIME_TEST = 'e2b_consumption_tests::lazy_product::e2b_lazy_consumption_server'
READY = 'e2b-lazy-product-ready:'
PROOF = 'e2b-lazy-product-proof:'


def require_lazy_owned_database(name, comment, owner):
    if (not isinstance(name, str) or not name.startswith('test_')
            or not isinstance(owner, str) or not owner or len(owner) > 128
            or not owner.isascii() or not all(value.isalnum() or value in '-_' for value in owner)
            or comment != 'centaeris:e2b-lazy-product:' + owner):
        raise ValueError('explicit owned synthetic product database required')


if os.environ.get('CENTAERIS_E2B_LAZY_PRODUCT_ACCEPTANCE') == '1':
    class E2bLazyConsumptionTests(LiveServerTestCase):
        serialized_rollback = True
        load_worker = native_fixture.NativeConsumptionProductionTests.load_worker
        claim_due_job = native_fixture.NativeConsumptionProductionTests.claim_due_job

        def setUp(self):
            self.assertEqual(sys.platform, 'linux', 'synthetic-provider product acceptance requires Linux')
            self.require_database_owner()
            isolate_runtime_schema(self)
            self.user = get_user_model().objects.create_user(username='e2b-lazy-product-owner')
            self.workspace = Workspace.objects.create(name='Managed workspace', createdBy=self.user)
            WorkspaceMembership.objects.create(workspace=self.workspace, user=self.user, role='owner')
            self.model = ModelConfig.objects.create(displayName='Text-only deterministic model')
            self.agent = Agent.objects.create(workspace=self.workspace, owner=self.user,
                name='Text-only agent', instructions='Answer directly.', model_config=self.model)
            self.client.force_login(self.user)
            self.requests, self.steps = [], []
            self.runtime_lines = deque(maxlen=80)
            self.runtime_markers = queue.Queue()
            self.runtime_lock = threading.Lock()
            self.runtime_ready = False
            self.runtime_url = self.start_runtime()
            with patch.dict(os.environ, {'AGENT_RUN_EXECUTION_MODE': 'perRun'}):
                self.worker = self.load_worker(self.runtime_url)

        def require_database_owner(self):
            database = connection.settings_dict
            self.assertTrue(database['NAME'].startswith('test_'))
            with connection.cursor() as cursor:
                cursor.execute("SELECT current_database(),shobj_description(oid,'pg_database') FROM pg_database WHERE datname=current_database()")
                name, comment = cursor.fetchone()
            owner = os.environ.get('CENTAERIS_E2B_LAZY_PRODUCT_DB_OWNER')
            require_lazy_owned_database(name, comment, owner)
            self.assertEqual(name, database['NAME'])
            return name, owner

        def start_runtime(self):
            argv = json.loads(os.environ['CENTAERIS_E2B_LAZY_RUNTIME_TEST_ARGV'])
            self.assertIsInstance(argv, list)
            self.assertTrue(all(isinstance(value, str) and value and '\x00' not in value for value in argv))
            self.assertIn(RUNTIME_TEST, argv)
            self.assertTrue({'--ignored', '--exact', '--nocapture'}.issubset(argv))
            database = connection.settings_dict
            name, owner = self.require_database_owner()
            database_url = 'postgresql://{}:{}@{}:{}/{}'.format(quote(database['USER']),
                quote(database['PASSWORD']), database['HOST'], database['PORT'], quote(name))
            fixture = {'schema': 'runtime.e2b_lazy_product.fixture.v1', 'databaseUrl': database_url,
                'databaseName': name, 'databaseOwner': owner,
                'workspaceId': self.workspace.pk, 'userId': str(self.user.pk), 'agentId': self.agent.pk}
            environment = {**os.environ, 'CENTAERIS_E2B_LAZY_PRODUCT_FIXTURE': json.dumps(fixture),
                'RUNTIME_E2B_PYTHON': sys.executable,
                'RUNTIME_E2B_ADAPTER_PACKAGE_ROOT': str(Path(__file__).resolve().parents[2] / 'e2b_adapter'),
                'API_INTERNAL_URL': self.live_server_url, 'INTERNAL_API_TOKEN': settings.INTERNAL_API_TOKEN,
                'AGENT_RUN_AUTHORIZATION_SIGNING_KEY': settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY,
                'REDIS_URL': 'redis://127.0.0.1:1/15', 'RUNTIME_STREAM_TTL_SECONDS': '3600',
                'RUNTIME_LIVE_STATE_TTL_SECONDS': '3600', 'NO_PROXY': '127.0.0.1,localhost'}
            environment.pop('CENTAERIS_NATIVE_CONSUME_PRODUCTION_FIXTURE', None)
            self.runtime_process = subprocess.Popen(argv, cwd=Path(__file__).resolve().parents[3],
                env=environment, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                text=True, encoding='utf-8')
            self.addCleanup(self.stop_runtime)
            def collect():
                for line in self.runtime_process.stdout:
                    self.runtime_lines.append(line)
                    if line.startswith((READY, PROOF)):
                        self.runtime_markers.put(line.rstrip())
                self.runtime_markers.put('process-exited')
            self.runtime_reader = threading.Thread(target=collect, daemon=True)
            self.runtime_reader.start()
            marker = self.runtime_markers.get(timeout=30)
            self.assertTrue(marker.startswith(READY), 'Synthetic product seam did not start: ' + ''.join(self.runtime_lines))
            self.runtime_ready = True
            return marker.removeprefix(READY)

        def stop_runtime(self):
            with self.runtime_lock:
                if self.runtime_process.poll() is None:
                    self.runtime_process.stdin.write('stop\n')
                    self.runtime_process.stdin.flush()
            self.runtime_process.wait(timeout=30)
            self.runtime_reader.join(timeout=5)
            self.runtime_process.stdin.close()
            self.runtime_process.stdout.close()
            if self.runtime_ready:
                self.assertEqual(self.runtime_process.returncode, 0, ''.join(self.runtime_lines))

        def runtime_proof(self, action, **values):
            with self.runtime_lock:
                self.assertIsNone(self.runtime_process.poll(), ''.join(self.runtime_lines))
                self.runtime_process.stdin.write(json.dumps({'schema': 'runtime.e2b_lazy_product.control.v1', 'action': action, **values}) + '\n')
                self.runtime_process.stdin.flush()
                marker = self.runtime_markers.get(timeout=10)
                self.assertTrue(marker.startswith(PROOF), ''.join(self.runtime_lines))
                proof = json.loads(marker.removeprefix(PROOF))
                self.assertEqual(proof['schema'], 'runtime.e2b_lazy_product.proof.v1')
                self.assertEqual(proof['action'], action)
                self.assertEqual(set(proof), {'schema', 'action', 'sdkLaunches', 'syntheticCreateCalls'}
                    | ({'attemptId'} if action == 'seedUnknown' else set()))
                return proof

        def admit(self):
            response = self.client.post(f'/api/workspaces/{self.workspace.pk}/sessions/new/messages',
                data=json.dumps({'operationId': 'managed-text-only', 'text': 'Return one text answer.',
                    'agentId': self.agent.pk, 'modelConfigRef': self.model.pk, 'attachmentRefs': []}),
                content_type='application/json')
            self.assertEqual(response.status_code, 202, response.content)
            run = AgentRun.objects.get(pk=response.json()['agentRunId'])
            profile = run.authorization.payload['environmentProfile']
            self.assertEqual((profile['lifetime'], profile['workspaceInitialization']), ('perRun', 'managedFiles'))
            return run

        def text_response(self, model, request):
            self.assertEqual(model.pk, self.model.pk)
            self.requests.append(deepcopy(request))
            self.assertEqual(self.runtime_proof('verify')['sdkLaunches'], 0)
            return {'text': 'Text-only managed workspace completed.', 'toolCalls': [], 'usage': zero_usage()}

        def operation_rows(self, run):
            with connection.cursor() as cursor:
                cursor.execute('SELECT to_jsonb(p)::text FROM runtime.provider_creation_operations p WHERE agent_run_id=%s ORDER BY attempt_id', [run.pk])
                return [json.loads(row[0]) for row in cursor.fetchall()]

        def test_text_only_final_uses_no_sdk_or_creation_and_real_no_effects_cleanup(self):
            with override_settings(RUNTIME_URL=self.runtime_url), \
                 patch('app_core.model_adapter.fake_model_response', side_effect=self.text_response):
                run = self.admit()
                job_id = 'agent_run.lifecycle:' + run.pk
                original = self.worker.agent_run_step_request
                def step(body):
                    result = original(body)
                    self.steps.append((deepcopy(body), deepcopy(result)))
                    return result
                def observe():
                    run.refresh_from_db()
                    return run.status, self.worker.runtime_request('/internal/jobs/' + job_id)['job']['status']
                with patch.object(self.worker, 'agent_run_step_request', side_effect=step):
                    drive_lifecycle_job(self.worker, job_id,
                        lambda: self.claim_due_job(self.worker, job_id, 'agent_run.lifecycle', 'worker:managed-text'),
                        observe, diagnostics=lambda: tuple(self.runtime_lines))
                self.assertEqual(len(self.requests), 1)
                self.assertEqual(len(self.steps), 1)
                self.assertEqual(self.steps[0][1]['terminalState'], 'completed')
                self.assertEqual(run.events.filter(payload__type='agent_run_completed').count(), 1)
                self.assertFalse(run.events.filter(payload__type='tool_call').exists())
                self.assertEqual(self.operation_rows(run), [])
                with connection.cursor() as cursor:
                    cursor.execute('SELECT receipt_id,allocation_kind,instance_id,physical_removed FROM runtime.agent_run_cleanup_receipts WHERE agent_run_id=%s', [run.pk])
                    receipt_id, kind, instance, removed = cursor.fetchone()
                    self.assertEqual((kind, instance, removed), ('neverAllocated', None, False))
                    cursor.execute('SELECT count(*) FROM runtime.resident_sandboxes')
                    self.assertEqual(cursor.fetchone()[0], 0)
                receipt = self.worker.runtime_request('/internal/agent-runs/cleanup-receipt', {
                    'schema': 'runtime.agent_run.cleanup_receipt.get.v2', 'agentRunId': run.pk,
                    'authorizationDigest': run.authorization.digest, 'cleanupReceiptId': receipt_id})
                self.assertEqual(receipt['schema'], 'runtime.agent_run.cleanup_receipt.no_provider_effects.v1')
                self.assertEqual(receipt['scope'], 'noProviderEffectsForRun')
                self.assertIs(receipt['cleanupComplete'], True)
                self.assertTrue(AgentRunEnvironmentRelease.objects.filter(agent_run=run).exists())
                session = Session.objects.get(pk=run.session_id)
                self.assertEqual((session.workspaceGeneration, session.workspaceSnapshotSizeBytes,
                    session.workspaceExpandedSizeBytes, session.workspaceFileCount), (0, 0, 0, 0))
                proof = self.runtime_proof('verify')
                self.assertEqual((proof['sdkLaunches'], proof['syntheticCreateCalls']), (0, 0))

        def test_dispatched_unknown_reopens_eager_without_new_dispatch_or_false_no_effects(self):
            with override_settings(RUNTIME_URL=self.runtime_url), \
                 patch('app_core.model_adapter.fake_model_response', side_effect=self.text_response):
                run = self.admit()
                job_id = 'agent_run.lifecycle:' + run.pk
                job = self.claim_due_job(self.worker, job_id, 'agent_run.lifecycle', 'worker:managed-unknown')
                self.worker.start_job(job_id, job['leaseOwner'])
                original = self.worker.agent_run_step_request
                before = []
                def step(body):
                    proof = self.runtime_proof('seedUnknown', step=body)
                    self.assertEqual(proof['sdkLaunches'], 0)
                    before.extend(self.operation_rows(run))
                    self.assertEqual(len(before), 1)
                    self.assertEqual(before[0]['attempt_id'], proof['attemptId'])
                    self.assertEqual(proof['syntheticCreateCalls'], 1)
                    self.assertEqual(before[0]['dispatch_state'], 'dispatched')
                    self.assertIsNone(before[0]['reported_sandbox_id'])
                    self.assertIsNone(before[0]['settlement_id'])
                    result = original(body)
                    self.steps.append((deepcopy(body), deepcopy(result)))
                    return result
                with patch.object(self.worker, 'agent_run_step_request', side_effect=step):
                    with self.assertRaises(self.worker.RetainedCleanupPending):
                        self.worker.execute_agent_run_lifecycle_job(job, job['leaseOwner'], lambda: None)
                self.assertEqual(len(self.steps), 1, 'the unknown case is one bounded product step')
                self.assertEqual(self.steps[0][1]['terminalState'], 'failed')
                self.assertEqual(self.steps[0][1]['transitionReason'], 'sandbox_prepare_failed')
                self.assertEqual(self.requests, [], 'unknown preparation must precede every model request')
                self.assertEqual(self.operation_rows(run), before, 'preserve the full original dispatched ledger row')
                with connection.cursor() as cursor:
                    cursor.execute('SELECT count(*) FROM runtime.agent_run_cleanup_receipts WHERE agent_run_id=%s', [run.pk])
                    self.assertEqual(cursor.fetchone()[0], 0)
                    cursor.execute('SELECT count(*) FROM runtime.provider_run_cleanup_receipts WHERE agent_run_id=%s', [run.pk])
                    self.assertEqual(cursor.fetchone()[0], 0)
                self.assertFalse(AgentRunEnvironmentRelease.objects.filter(agent_run=run).exists())
                self.assertNotEqual(self.worker.runtime_request('/internal/jobs/' + job_id)['job']['status'], 'succeeded')
                proof = self.runtime_proof('verify')
                self.assertEqual((proof['sdkLaunches'], proof['syntheticCreateCalls']), (0, 1))
