"""Public submission pins authorization without excluding another environment's physical work."""
import json
from concurrent.futures import ThreadPoolExecutor, TimeoutError as FutureTimeout
from threading import Event
import time
from unittest.mock import patch

from django.db import DatabaseError, close_old_connections, connection, transaction
from django.test import Client, TransactionTestCase

from .agent_run_authorization_factory import create_agent_run_authorization
from .models import AgentRun, AgentRunAuthorization, AgentRunEnvironmentRelease, HostedOperationReceipt, Session, Workspace, WorkspaceMembership
from .test_hosted_operations import OperationFixture
from .testing import create_session


class PublicSubmissionLockTests(OperationFixture, TransactionTestCase):
    serialized_rollback = True

    def client_for_user(self):
        client = Client()
        client.force_login(self.user)
        return client

    def submit(self, client, *, operation='public-submit', session='new', text='hello'):
        return client.post(self.create_url + '/' + session + '/messages', content_type='application/json',
            data=json.dumps({'operationId': operation,
                'modelConfigRef': self.model.pk, 'text': text}))

    @staticmethod
    def independently(operation):
        close_old_connections()
        try:
            with connection.cursor() as cursor:
                cursor.execute('SHOW lock_timeout')
                if cursor.fetchone()[0] != '0':
                    raise AssertionError('public concurrency proof requires default lock_timeout=0')
            return operation()
        finally:
            connection.close()

    def observed(self, operation, identity):
        def observe(execute, sql, params, many, context):
            if not identity.get('capturing') and 'pid' not in identity:
                identity['capturing'] = True
                with connection.cursor() as cursor:
                    cursor.execute('SELECT pg_backend_pid()')
                    identity['pid'] = cursor.fetchone()[0]
                identity['started'].set()
            return execute(sql, params, many, context)
        with connection.execute_wrapper(observe):
            return operation()

    def wait_for_blocker(self, future, identity, expected_pid=None):
        self.assertTrue(identity['started'].wait(5))
        end = time.monotonic() + 5
        while time.monotonic() < end:
            if future.done():
                response = future.result()
                self.fail('contender completed instead of waiting for the uncommitted transaction: ' + str(response.status_code))
            with connection.cursor() as cursor:
                cursor.execute('SELECT pg_blocking_pids(%s)', [identity['pid']])
                blockers = cursor.fetchone()[0]
            if blockers and (expected_pid is None or expected_pid in blockers):
                return
            time.sleep(.01)
        self.fail('contender did not reach its real PostgreSQL lock wait')

    def run_while_locked(self, statements, operation):
        timed_out = False
        with ThreadPoolExecutor(max_workers=1) as pool:
            with transaction.atomic(), connection.cursor() as cursor:
                for sql, parameters in statements:
                    cursor.execute(sql, parameters)
                future = pool.submit(self.independently, operation)
                try:
                    response = future.result(timeout=2)
                except FutureTimeout:
                    timed_out = True
            if timed_out:
                future.result(timeout=10)
                self.fail('public submission waited for another environment to release its shared authorization')
        return response

    def test_runtime_shared_authorization_allows_another_session_public_submission(self):
        original = create_session(workspace=self.workspace, owner=self.user)
        run = AgentRun.objects.create(workspace=self.workspace, user=self.user, session=original,
            modelConfig=self.model, prompt='physical operation', status='running',
            agent_instructions="")
        create_agent_run_authorization(run, image_digest='sha256:' + 'a' * 64)
        target = create_session(workspace=self.workspace, owner=self.user)
        client = self.client_for_user()
        locks = [('SELECT id FROM app_core_workspace WHERE id=%s FOR SHARE', [self.workspace.pk]),
            ('SELECT id FROM app_core_workspacemembership WHERE id=%s FOR SHARE', [run.membership_ref]),
            ('SELECT id FROM app_core_session WHERE id=%s FOR SHARE', [original.pk]),
            ('SELECT id FROM app_core_sessionenvironment WHERE id=%s FOR SHARE', [run.environment_id]),
            ('SELECT id FROM app_core_agentrun WHERE id=%s FOR SHARE', [run.pk]),
            ('SELECT agent_run_id FROM app_core_agentrunauthorization WHERE agent_run_id=%s FOR SHARE', [run.pk])]
        for session_id in ['new', target.pk]:
            with self.subTest(session=session_id):
                response = self.run_while_locked(locks,
                    lambda: self.submit(client, operation='shared-' + session_id, session=session_id))
                self.assertEqual(response.status_code, 202, response.content)
                if session_id != 'new':
                    self.assertEqual(response.json()['sessionId'], target.pk)
        self.assertEqual(HostedOperationReceipt.objects.filter(command='submitMessage').count(), 2)
        self.assertEqual(AgentRun.objects.filter(session=target).count(), 1)
        self.assertEqual(self.schedule.call_count, 2)

    def test_same_operation_with_different_inputs_has_one_receipt_and_exact_conflict(self):
        first_client, second_client = self.client_for_user(), self.client_for_user()
        locked, release = Event(), Event()
        first_identity, second_identity = {'started': Event()}, {'started': Event()}
        def pause_authorization(run, *args, **kwargs):
            result = create_agent_run_authorization(run, *args, **kwargs)
            if run.prompt == "hello":
                locked.set()
                if not release.wait(10):
                    raise AssertionError('test did not release first submission')
            return result
        with patch('app_core.http.workspaces.create_agent_run_authorization', side_effect=pause_authorization):
            with ThreadPoolExecutor(max_workers=2) as pool:
                first = pool.submit(self.independently, lambda: self.observed(
                    lambda: self.submit(first_client, operation='same-operation'), first_identity))
                try:
                    self.assertTrue(locked.wait(5))
                    second = pool.submit(self.independently,
                        lambda: self.observed(lambda: self.submit(second_client, operation='same-operation', text='different input'), second_identity))
                    self.wait_for_blocker(second, second_identity, first_identity['pid'])
                finally:
                    release.set()
                responses = [first.result(timeout=10), second.result(timeout=10)]
        self.assertEqual([response.status_code for response in responses], [202, 409])
        self.assertEqual(responses[1].json(), {'error': 'operation_conflict'})
        self.assertEqual(Session.objects.count(), 1)
        self.assertEqual(AgentRun.objects.count(), 1)
        self.assertEqual(HostedOperationReceipt.objects.count(), 1)
        self.schedule.assert_called_once()

    def test_waiting_for_one_session_does_not_block_independent_admission(self):
        target = create_session(workspace=self.workspace, owner=self.user)
        first_client, second_client = self.client_for_user(), self.client_for_user()
        identity = {'started': Event()}
        with ThreadPoolExecutor(max_workers=2) as pool:
            with transaction.atomic(), connection.cursor() as cursor:
                cursor.execute('SELECT pg_backend_pid()')
                holder_pid = cursor.fetchone()[0]
                cursor.execute('SELECT id FROM app_core_session WHERE id=%s FOR SHARE', [target.pk])
                first = pool.submit(self.independently, lambda: self.observed(
                    lambda: self.submit(first_client, session=target.pk, operation='session-wait'), identity))
                self.wait_for_blocker(first, identity, holder_pid)
                second = pool.submit(self.independently,
                    lambda: self.submit(second_client, operation='independent-session'))
                try:
                    response = second.result(timeout=2)
                except FutureTimeout:
                    response = None
            first_response, second_response = first.result(timeout=10), second.result(timeout=10)
        self.assertIsNotNone(response, 'another Session wait excluded independent admission')
        self.assertEqual((first_response.status_code, second_response.status_code), (202, 202))
        self.assertEqual(HostedOperationReceipt.objects.count(), 2)

    def test_same_session_parallel_operations_cannot_create_two_active_runs(self):
        from threading import Barrier
        target = create_session(workspace=self.workspace, owner=self.user)
        clients = [self.client_for_user(), self.client_for_user()]
        barrier = Barrier(2)
        self.profile.side_effect = lambda: (barrier.wait(5), {'schema': 'runtime.execution_profile.v1',
            'imageDigest': 'sha256:' + 'a' * 64})[1]
        with ThreadPoolExecutor(max_workers=2) as pool:
            futures = [pool.submit(self.independently, lambda index=index: self.submit(clients[index],
                session=target.pk, operation='same-session-' + str(index))) for index in range(2)]
            responses = [future.result(timeout=10) for future in futures]
        self.assertEqual(sorted(response.status_code for response in responses), [202, 409])
        self.assertEqual(next(response.json() for response in responses if response.status_code == 409),
            {'error': 'session_has_active_agent_run'})
        self.assertEqual(AgentRun.objects.filter(session=target, status__in=['queued', 'running']).count(), 1)
        self.assertEqual(HostedOperationReceipt.objects.count(), 1)
        self.schedule.assert_called_once()

    def test_terminal_workspace_handoff_blocks_submission_until_release_and_freezes_committed_baseline(self):
        from django.conf import settings
        from .runtime_contract import verify_agent_run_authorization_signature

        for status in ('completed', 'failed', 'cancelled'):
            with self.subTest(status=status):
                target = create_session(workspace=self.workspace, owner=self.user)
                run = AgentRun.objects.create(workspace=self.workspace, user=self.user, session=target,
                    modelConfig=self.model, prompt='original', status=status, agent_instructions='')
                create_agent_run_authorization(run, image_digest='sha256:' + 'a' * 64)
                client = self.client_for_user()
                operation = 'handoff-' + status
                counts = (AgentRun.objects.count(), AgentRunAuthorization.objects.count(),
                    HostedOperationReceipt.objects.count())
                response = self.submit(client, session=target.pk, operation=operation)
                self.assertEqual((response.status_code, response.json()),
                    (409, {'error': 'session_workspace_preservation_pending'}))
                self.assertEqual((AgentRun.objects.count(), AgentRunAuthorization.objects.count(),
                    HostedOperationReceipt.objects.count()), counts)

                identity = {'started': Event()}
                with ThreadPoolExecutor(max_workers=1) as pool:
                    with transaction.atomic(), connection.cursor() as cursor:
                        cursor.execute('SELECT pg_backend_pid()')
                        holder_pid = cursor.fetchone()[0]
                        Session.objects.select_for_update().get(pk=target.pk)
                        # Final empty snapshot publication and cleanup release
                        # both pin this Session before a successor can freeze it.
                        Session.objects.filter(pk=target.pk).update(workspaceGeneration=1,
                            workspaceLastAdvancedAgentRun=run)
                        AgentRunEnvironmentRelease.objects.create(agent_run=run, environment=run.environment,
                            authorization_digest=run.authorization.digest, controller_epoch=1,
                            controller_lease_digest='sha256:' + 'b' * 64)
                        future = pool.submit(self.independently, lambda: self.observed(
                            lambda: self.submit(client, session=target.pk, operation=operation), identity))
                        self.wait_for_blocker(future, identity, holder_pid)
                    response = future.result(timeout=10)
                self.assertEqual(response.status_code, 202, response.content)
                successor = AgentRun.objects.get(pk=response.json()['agentRunId'])
                self.assertEqual(successor.authorization.payload['sessionWorkspace'], {
                    'generation': 1, 'snapshotSha256': '', 'snapshotSizeBytes': 0,
                    'expandedSizeBytes': 0, 'fileCount': 0})
                verify_agent_run_authorization_signature(successor.authorization.payload,
                    settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY, successor.authorization.signature)

    def test_terminal_handoff_does_not_block_another_session(self):
        original = create_session(workspace=self.workspace, owner=self.user)
        run = AgentRun.objects.create(workspace=self.workspace, user=self.user, session=original,
            modelConfig=self.model, prompt='cleanup pending', status='failed', agent_instructions='')
        create_agent_run_authorization(run, image_digest='sha256:' + 'a' * 64)
        target = create_session(workspace=self.workspace, owner=self.user)
        response = self.submit(self.client_for_user(), session=target.pk)
        self.assertEqual(response.status_code, 202, response.content)


    def test_authorization_pin_blocks_non_key_workspace_status_and_member_role_writes(self):
        locked, release = Event(), Event()
        client = self.client_for_user()
        def pause_authorization(run, *args, **kwargs):
            result = create_agent_run_authorization(run, *args, **kwargs)
            locked.set()
            if not release.wait(10):
                raise AssertionError('test did not release authorization transaction')
            return result
        member = WorkspaceMembership.objects.get(workspace=self.workspace, user=self.user)
        with patch('app_core.http.workspaces.create_agent_run_authorization', side_effect=pause_authorization):
            with ThreadPoolExecutor(max_workers=1) as pool:
                submitter = pool.submit(self.independently, lambda: self.submit(client))
                try:
                    self.assertTrue(locked.wait(5))
                    for model, identity, fields in [(Workspace, self.workspace.pk, {'status': 'inactive'}),
                        (WorkspaceMembership, member.pk, {'role': 'admin'})]:
                        with self.subTest(model=model.__name__):
                            with self.assertRaises(DatabaseError) as raised:
                                with transaction.atomic(), connection.cursor() as cursor:
                                    cursor.execute("SET LOCAL lock_timeout='200ms'")
                                    model.objects.filter(pk=identity).update(**fields)
                            self.assertEqual(raised.exception.__cause__.sqlstate, '55P03')
                finally:
                    release.set()
                response = submitter.result(timeout=10)
        self.assertEqual(response.status_code, 202, response.content)
