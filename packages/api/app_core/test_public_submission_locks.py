"""Public submission pins authorization without excluding another environment's physical work."""
import json
from concurrent.futures import ThreadPoolExecutor, TimeoutError as FutureTimeout
from threading import Event
import time
from unittest.mock import patch

from django.db import DatabaseError, close_old_connections, connection, transaction
from django.test import Client, TransactionTestCase

from .agent_run_authorization_factory import create_agent_run_authorization
from .models import Agent, AgentCoordinationSession, AgentRun, HostedOperationReceipt, Session, Workspace, WorkspaceMembership
from .test_hosted_operations import OperationFixture
from .testing import create_session, create_shared_agent


class PublicSubmissionLockTests(OperationFixture, TransactionTestCase):
    serialized_rollback = True

    def client_for_user(self):
        client = Client()
        client.force_login(self.user)
        return client

    def submit(self, client, *, operation='public-submit', agent=None, session='new', text='hello'):
        return client.post(self.create_url + '/' + session + '/messages', content_type='application/json',
            data=json.dumps({'operationId': operation, 'agentId': (agent or self.agent).pk,
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
        original = create_session(workspace=self.workspace, owner=self.user, agent=self.agent)
        run = AgentRun.objects.create(workspace=self.workspace, user=self.user, session=original,
            modelConfig=self.model, prompt='physical operation', status='running',
            definition_version=self.agent.definition.published_version,
            agent_instructions=self.agent.definition.published_version.instructions)
        create_agent_run_authorization(run, image_digest='sha256:' + 'a' * 64)
        target = create_session(workspace=self.workspace, owner=self.user, agent=self.agent)
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

    def test_same_operation_with_different_agent_payloads_has_one_receipt_and_exact_conflict(self):
        other = create_shared_agent(workspace=self.workspace, owner=self.user, name='Other target', model_config=self.model)
        first_client, second_client = self.client_for_user(), self.client_for_user()
        locked, release = Event(), Event()
        first_identity, second_identity = {'started': Event()}, {'started': Event()}
        def pause_authorization(run, *args, **kwargs):
            result = create_agent_run_authorization(run, *args, **kwargs)
            if run.session.agent_id == self.agent.pk:
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
                        lambda: self.observed(lambda: self.submit(second_client, operation='same-operation', agent=other), second_identity))
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

    def test_queue_contention_has_no_side_effects_and_the_same_operation_retries(self):
        client = self.client_for_user()
        response = self.run_while_locked([('SELECT pg_advisory_xact_lock(731946,1)', [])],
            lambda: self.submit(client, operation='queue-retry'))
        self.assertEqual(response.status_code, 503, response.content)
        self.assertEqual(response.json(), {'error': 'execution_admission_busy'})
        self.assertEqual(response['Retry-After'], '5')
        self.assertFalse(Session.objects.exists())
        self.assertFalse(AgentRun.objects.exists())
        self.assertFalse(HostedOperationReceipt.objects.exists())
        self.schedule.assert_not_called()
        response = self.submit(client, operation='queue-retry')
        self.assertEqual(response.status_code, 202, response.content)
        self.assertEqual((Session.objects.count(), AgentRun.objects.count(), HostedOperationReceipt.objects.count()), (1, 1, 1))
        self.schedule.assert_called_once()

    def test_waiting_for_one_session_does_not_hold_the_global_admission_lock(self):
        target = create_session(workspace=self.workspace, owner=self.user, agent=self.agent)
        other = create_shared_agent(workspace=self.workspace, owner=self.user, name='Independent agent', model_config=self.model)
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
                    lambda: self.submit(second_client, agent=other, operation='independent-agent'))
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
        target = create_session(workspace=self.workspace, owner=self.user, agent=self.agent)
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

    def test_coordination_binding_committed_during_profile_is_rejected_before_run_or_receipt(self):
        from .test_hosted_operations import PROFILE
        from .workspace_access import locked_workspace_membership_for
        target = create_session(workspace=self.workspace, owner=self.user, agent=self.agent)
        def bind():
            with transaction.atomic():
                self.assertIsNotNone(locked_workspace_membership_for(self.user, self.workspace.pk))
                agent = Agent.objects.select_for_update().get(pk=self.agent.pk)
                session = Session.objects.select_for_update().get(pk=target.pk)
                AgentCoordinationSession.objects.create(agent=agent, session=session)
        def profile():
            self.assertFalse(connection.in_atomic_block, 'profile RPC cannot retain submission locks')
            with ThreadPoolExecutor(max_workers=1) as pool:
                pool.submit(self.independently, bind).result(timeout=5)
            return PROFILE
        self.profile.side_effect = profile
        response = self.submit(self.client, session=target.pk, operation='became-coordination')
        self.assertEqual(response.status_code, 409, response.content)
        self.assertEqual(response.json(), {'error': 'coordination_session_requires_agent_input'})
        self.assertFalse(AgentRun.objects.exists())
        self.assertFalse(HostedOperationReceipt.objects.exists())
        self.assertEqual(AgentCoordinationSession.objects.count(), 1)
        self.schedule.assert_not_called()

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
