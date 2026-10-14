import json
from unittest.mock import patch

from django.db import connection
from django.test import SimpleTestCase, TransactionTestCase
from django.utils import timezone

from .models import AgentRun, SessionEvent, WorkspaceMembership
from .test_execution_publication_fencing import ExecutionPublicationFixture


class RetainedLifecycleTests(ExecutionPublicationFixture, TransactionTestCase):
    serialized_rollback = True

    def post(self, path='resolve', **changes):
        body = {'schema': 'runtime.agent_run_lifecycle.resolve.v2', 'jobId': self.job_id,
            'leaseOwner': self.owner, 'agentRunId': self.run.pk,
            'authorizationDigest': self.authorization.digest} | changes
        return self.client.post('/internal/agent-run-lifecycle/' + path, data=json.dumps(body),
            content_type='application/json', HTTP_X_INTERNAL_TOKEN='test-internal-token')

    def terminal(self):
        AgentRun.objects.filter(pk=self.run.pk).update(preAdmissionCancelledAt=timezone.now())

    def test_committed_terminal_is_projected_before_durable_cleanup_without_inputs(self):
        self.terminal()
        WorkspaceMembership.objects.filter(pk=self.run.membership_ref).delete()
        with patch('app_core.http.internal.build_agent_run_start', side_effect=AssertionError('execute builder called')):
            result = self.post()
        self.assertEqual(result.status_code, 200, result.content)
        payload = result.json()
        self.assertEqual(set(payload), {'schema', 'disposition', 'terminalState', 'agentRunCleanup'})
        cleanup = payload['agentRunCleanup']
        self.assertEqual(set(cleanup), {'schema', 'agentRunId', 'authorizationDigest',
            'authorizationSignature', 'authorization', 'environmentAuthorization'})
        self.assertEqual(cleanup['environmentAuthorization']['payload']['controllerPurpose'], 'cleanup')
        self.run.refresh_from_db()
        self.assertEqual(self.run.status, 'cancelled')

    def test_status_without_core_terminal_cannot_authorize_cleanup(self):
        AgentRun.objects.filter(pk=self.run.pk).update(status='failed')
        response = self.post()
        self.assertEqual(response.status_code, 409)
        self.assertEqual(response.json()['error'], 'agent_run_semantic_terminal_missing')
        self.session.current_environment.refresh_from_db()
        self.assertEqual(self.session.current_environment.controller_epoch, 0)

    def test_core_terminal_event_overrides_running_projection(self):
        now_ms = int(timezone.now().timestamp() * 1000)
        SessionEvent.objects.create(eventId='terminal-event', workspace_id=self.run.workspace_id,
            session_id=self.run.session_id, agent_run=self.run, sequence=1, agent_run_sequence=1,
            projects_to_agent_run_stream=True, createdAtMs=now_ms,
            payload={'type': 'agent_run_completed', 'createdAtMs': now_ms,
                'payload': {'doneReason': 'finalized'}})
        with patch('app_core.http.internal.build_agent_run_start', side_effect=AssertionError('execute builder called')):
            result = self.post()
        self.assertEqual(result.status_code, 200, result.content)
        self.assertEqual(result.json()['terminalState'], 'completed')
        self.run.refresh_from_db()
        self.assertEqual(self.run.status, 'completed')

    def test_final_committed_during_acquire_resolves_cleanup_instead_of_execute(self):
        from .environment_lifecycle import acquire_environment_controller
        def concurrent_final(*args):
            self.terminal()
            return acquire_environment_controller(*args)
        with patch('app_core.http.internal.acquire_environment_controller', side_effect=concurrent_final, create=True):
            response = self.post()
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json()['disposition'], 'terminal')
        self.assertEqual(response.json()['agentRunCleanup']['environmentAuthorization']['payload']['controllerPurpose'], 'cleanup')

    def test_released_history_replay_does_not_reacquire_or_read_runtime(self):
        from .environment_lifecycle import acquire_environment_cleanup_controller, release_environment_controller
        self.terminal()
        AgentRun.objects.filter(pk=self.run.pk).update(status='cancelled')
        authority = acquire_environment_cleanup_controller(self.run.pk, self.owner)
        receipt = release_environment_controller(self.run.pk, authority['controllerEpoch'], self.owner)
        next_run = AgentRun.objects.create(workspace_id=self.run.workspace_id, session=self.session,
            user_id=self.run.user_id, modelConfig_id=self.run.modelConfig_id, prompt='next controller')
        environment = self.session.current_environment
        environment.controller_run = next_run
        environment.controller_epoch = authority['controllerEpoch'] + 1
        environment.controller_purpose = 'execute'
        environment.controller_released = False
        environment.save(update_fields=['controller_run', 'controller_epoch', 'controller_purpose', 'controller_released'])
        with patch('app_core.http.internal.acquire_environment_cleanup_controller', side_effect=AssertionError('reacquire'), create=True):
            response = self.post()
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json()['disposition'], 'released')
        self.assertEqual(response.json()['releaseReceiptId'], receipt['releaseReceiptId'])
        environment.refresh_from_db()
        self.assertEqual(environment.controller_run_id, next_run.pk)
        self.assertEqual(environment.controller_epoch, authority['controllerEpoch'] + 1)
        self.assertFalse(environment.controller_released)

    def test_incomplete_runtime_receipt_cannot_release_controller(self):
        response = self.post()
        self.assertEqual(response.status_code, 200, response.content)
        self.terminal()
        cleanup = self.post().json()['agentRunCleanup']['environmentAuthorization']['payload']
        with patch('app_core.http.internal.get_agent_run_cleanup_receipt', return_value={
                'schema': 'runtime.agent_run.cleanup_receipt.v1', 'receiptId': 'cleanup-1',
                'agentRunId': self.run.pk, 'environmentId': self.run.environment_id, 'instanceId': 'instance-1',
                'authorizationDigest': self.authorization.digest, 'controllerEpoch': cleanup['controllerEpoch'],
                'cleanupComplete': False}, create=True):
            result = self.post('release', schema='runtime.agent_run_lifecycle.release.v1',
                controllerEpoch=cleanup['controllerEpoch'], cleanupReceiptId='cleanup-1')
        self.assertEqual(result.status_code, 409)
        self.session.current_environment.refresh_from_db()
        self.assertFalse(self.session.current_environment.controller_released)

    def test_invalid_lease_and_unknown_fields_reject_before_controller_mutation(self):
        self.assertEqual(self.post(leaseOwner='another-worker').status_code, 409)
        self.assertEqual(self.post(extra=True).status_code, 400)
        self.session.current_environment.refresh_from_db()
        self.assertEqual(self.session.current_environment.controller_epoch, 0)

    def test_terminal_cleanup_deadletter_is_reported_without_new_execution_job(self):
        self.terminal()
        response = self.post()
        self.assertEqual(response.status_code, 200, response.content)
        request = {'schema': 'runtime.agent_run_lifecycle.reconcile.v2', 'limit': 10, 'after': None}
        with patch('app_core.http.internal.get_runtime_job', return_value={'status': 'dead_lettered'}), \
                patch('app_core.http.internal.schedule_agent_run_lifecycle', side_effect=AssertionError('new execution scheduled')):
            result = self.client.post('/internal/agent-run-lifecycle/reconcile', data=json.dumps(request),
                content_type='application/json', HTTP_X_INTERNAL_TOKEN='test-internal-token')
        self.assertEqual(result.status_code, 200, result.content)
        self.assertEqual(result.json()['pending'], [{'agentRunId': self.run.pk,
            'authorizationDigest': self.authorization.digest, 'terminalState': 'cancelled', 'jobStatus': 'dead_lettered'}])
        self.run.refresh_from_db()
        self.assertEqual(self.run.status, 'cancelled')
        self.session.current_environment.refresh_from_db()
        self.assertFalse(self.session.current_environment.controller_released)

        self.session.current_environment.controller_purpose = 'execute'
        self.session.current_environment.save(update_fields=['controller_purpose'])
        with patch('app_core.http.internal.get_runtime_job', return_value={'status': 'dead_lettered'}):
            before_cleanup = self.client.post('/internal/agent-run-lifecycle/reconcile', data=json.dumps(request),
                content_type='application/json', HTTP_X_INTERNAL_TOKEN='test-internal-token')
        self.assertEqual(len(before_cleanup.json()['pending']), 1,
            'terminal may deadletter before acquiring its cleanup fence')

    def test_previous_cleanup_receipt_survives_new_lease_and_future_receipt_is_rejected(self):
        self.terminal()
        old_grant = self.post().json()['agentRunCleanup']['environmentAuthorization']['payload']
        self._lease(self.replacement)
        self.owner = self.replacement
        grant = self.post().json()['agentRunCleanup']['environmentAuthorization']['payload']
        self.assertGreater(grant['controllerEpoch'], old_grant['controllerEpoch'])
        receipt = {'schema': 'runtime.agent_run.cleanup_receipt.v1', 'receiptId': 'cleanup-1',
            'agentRunId': self.run.pk, 'environmentId': self.run.environment_id, 'instanceId': 'instance-1',
            'authorizationDigest': self.authorization.digest, 'controllerEpoch': old_grant['controllerEpoch'], 'cleanupComplete': True}
        def release():
            return self.post('release', schema='runtime.agent_run_lifecycle.release.v1',
                controllerEpoch=grant['controllerEpoch'], cleanupReceiptId='cleanup-1')
        with patch('app_core.http.internal.get_agent_run_cleanup_receipt', return_value=receipt | {'controllerEpoch': grant['controllerEpoch'] + 1}):
            self.assertEqual(release().status_code, 409)
        with patch('app_core.http.internal.get_agent_run_cleanup_receipt', return_value=receipt):
            response = release()
        self.assertEqual(response.status_code, 200, response.content)
        release_id = response.json()['releaseReceiptId']
        with patch('app_core.http.internal.get_agent_run_cleanup_receipt', side_effect=AssertionError('released replay accessed Runtime')):
            self.assertEqual(release().json()['releaseReceiptId'], release_id)

    def test_lease_changed_during_receipt_read_cannot_release(self):
        self.terminal()
        grant = self.post().json()['agentRunCleanup']['environmentAuthorization']['payload']
        def changed_lease(*args):
            self.assertFalse(connection.in_atomic_block, 'network receipt read cannot hold projection transaction')
            self._lease(self.replacement)
            return {'environmentId': self.run.environment_id, 'controllerEpoch': grant['controllerEpoch'], 'cleanupComplete': True}
        with patch('app_core.http.internal.get_agent_run_cleanup_receipt', side_effect=changed_lease):
            response = self.post('release', schema='runtime.agent_run_lifecycle.release.v1',
                controllerEpoch=grant['controllerEpoch'], cleanupReceiptId='cleanup-1')
        self.assertEqual(response.status_code, 409)
        self.session.current_environment.refresh_from_db()
        self.assertFalse(self.session.current_environment.controller_released)


class CleanupReceiptContractTests(SimpleTestCase):
    def test_complete_immutable_receipt_is_required_and_identity_is_exact(self):
        from .runtime_client import get_agent_run_cleanup_receipt
        good = {'schema': 'runtime.agent_run.cleanup_receipt.v1', 'receiptId': 'receipt-1',
            'agentRunId': 'run-1', 'environmentId': 'env-1', 'allocation': {'kind': 'instance', 'instanceId': 'instance-1'},
            'authorizationDigest': 'sha256:' + 'a' * 64, 'controllerEpoch': 2, 'cleanupComplete': True}
        with patch('app_core.runtime_client.request_json', return_value=good) as transport:
            self.assertEqual(get_agent_run_cleanup_receipt('run-1', good['authorizationDigest'], 'receipt-1'), good)
            self.assertEqual(transport.call_args.args[0], '/internal/agent-runs/cleanup-receipt')
        with patch('app_core.runtime_client.request_json', return_value=good | {'allocation': {'kind': 'neverAllocated'}}):
            self.assertEqual(get_agent_run_cleanup_receipt('run-1', good['authorizationDigest'], 'receipt-1')['allocation'], {'kind': 'neverAllocated'})
        for mutation in ({'cleanupComplete': False}, {'cleanupComplete': 1}, {'receiptId': 'another'},
                {'agentRunId': 'another'}, {'authorizationDigest': 'sha256:' + 'b' * 64},
                {'allocation': {'kind': 'instance', 'instanceId': ''}}, {'allocation': {'kind': 'neverAllocated', 'instanceId': 'invented'}},
                {'allocation': {'kind': 'unknown'}}, {'allocation': None},
                {'controllerEpoch': True}, {'extra': True}, {'schema': 'unsupported'}):
            with self.subTest(mutation=mutation), patch('app_core.runtime_client.request_json', return_value=good | mutation):
                with self.assertRaisesRegex(ValueError, 'agent_run_cleanup_receipt_invalid'):
                    get_agent_run_cleanup_receipt('run-1', good['authorizationDigest'], 'receipt-1')
