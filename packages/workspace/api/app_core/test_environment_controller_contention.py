"""Real PostgreSQL coverage for short, compatible controller authorization locks."""
import json
from concurrent.futures import ThreadPoolExecutor, TimeoutError as FutureTimeout
from threading import Event
from unittest.mock import patch

from django.db import DatabaseError, close_old_connections, connection, transaction
from django.test import Client, TransactionTestCase
from django.utils import timezone

from .agent_run_authorization_factory import create_agent_run_authorization
from .environment_lifecycle import (
    EnvironmentControllerBusy,
    acquire_environment_cleanup_controller,
    acquire_environment_controller,
    release_environment_controller,
)
from .models import AgentRun, AgentRunEnvironmentRelease, SessionEnvironment, Workspace, WorkspaceMembership
from .test_execution_publication_fencing import ExecutionPublicationFixture
from .testing import create_session


class EnvironmentControllerContentionTests(ExecutionPublicationFixture, TransactionTestCase):
    serialized_rollback = True

    def state(self):
        environment = SessionEnvironment.objects.filter(pk=self.run.environment_id).values(
            'controller_epoch', 'controller_run_id', 'controller_maintenance_operation_id',
            'controller_lease_owner', 'controller_released', 'controller_purpose').get()
        return environment, AgentRunEnvironmentRelease.objects.filter(agent_run_id=self.run.pk).count()

    def post(self, path='resolve', *, run=None, authorization=None, **changes):
        run, authorization = run or self.run, authorization or self.authorization
        body = {'schema': 'runtime.agent_run_lifecycle.' + path + ('.resolve.v1' if path == 'control' else '.v1'),
            'jobId': 'agent_run.lifecycle:' + run.pk, 'leaseOwner': self.owner,
            'agentRunId': run.pk, 'authorizationDigest': authorization.digest} | changes
        if path == 'resolve':
            body.setdefault('executionMode', 'retained')
        return Client().post('/internal/agent-run-lifecycle/' + path, data=json.dumps(body),
            content_type='application/json', HTTP_X_INTERNAL_TOKEN='test-internal-token')

    @staticmethod
    def independently(operation):
        close_old_connections()
        try:
            with connection.cursor() as cursor:
                cursor.execute('SHOW lock_timeout')
                if cursor.fetchone()[0] != '0':
                    raise AssertionError('contention coverage requires the production default lock_timeout=0')
            return operation()
        finally:
            connection.close()

    def while_locked(self, statements, operation):
        # Release the holder before joining a stalled contender, so regressions
        # fail boundedly without leaving a blocked thread or test transaction.
        timed_out = False
        with ThreadPoolExecutor(max_workers=1) as pool:
            with transaction.atomic(), connection.cursor() as cursor:
                for sql, params in statements:
                    cursor.execute(sql, params)
                future = pool.submit(self.independently, operation)
                try:
                    result = future.result(timeout=2)
                except FutureTimeout:
                    timed_out = True
            if timed_out:
                future.result(timeout=10)
                self.fail('controller operation waited for the holder instead of completing while it remained locked')
        return result

    def lock_stages(self):
        return [
            ('workspace', 'app_core_workspace', 'id', self.run.workspace_id),
            ('membership', 'app_core_workspacemembership', 'id', self.run.membership_ref),
            ('session', 'app_core_session', 'id', self.run.session_id),
            ('environment', 'app_core_sessionenvironment', 'id', self.run.environment_id),
            ('run', 'app_core_agentrun', 'id', self.run.pk),
            ('grant', 'app_core_agentrunauthorization', 'agent_run_id', self.run.pk),
            ('job', 'runtime.runtime_jobs', 'job_id', self.job_id),
        ]

    def busy_result(self, operation):
        try:
            return operation()
        except EnvironmentControllerBusy as error:
            return str(error)

    def cleanup_authority(self):
        AgentRun.objects.filter(pk=self.run.pk).update(
            status='cancelled', preAdmissionCancelledAt=timezone.now())
        return acquire_environment_cleanup_controller(self.run.pk, self.owner)

    def test_every_authority_lock_is_bounded_and_cannot_commit_an_epoch(self):
        before = self.state()
        for stage, table, key, identity in self.lock_stages():
            with self.subTest(stage=stage):
                before = self.state()
                result = self.while_locked([(f'SELECT {key} FROM {table} WHERE {key}=%s FOR UPDATE', [identity])],
                    lambda: self.busy_result(lambda: acquire_environment_controller(self.run.pk, self.owner)))
                self.assertEqual(result, 'environment_controller_busy')
                self.assertEqual(self.state(), before)
        self.assertEqual(acquire_environment_controller(self.run.pk, self.owner)['controllerEpoch'], 1)

    def test_every_release_lock_is_bounded_and_cannot_commit_a_receipt(self):
        authority = self.cleanup_authority()
        before = self.state()
        for stage, table, key, identity in self.lock_stages():
            with self.subTest(stage=stage):
                before = self.state()
                result = self.while_locked([(f'SELECT {key} FROM {table} WHERE {key}=%s FOR UPDATE', [identity])],
                    lambda: self.busy_result(lambda: release_environment_controller(
                        self.run.pk, authority['controllerEpoch'], self.owner)))
                self.assertEqual(result, 'environment_controller_busy')
                self.assertEqual(self.state(), before)
        self.assertTrue(release_environment_controller(self.run.pk, authority['controllerEpoch'], self.owner)['released'])

    def test_another_session_can_resolve_while_runtime_holds_shared_authorization(self):
        session = create_session(workspace=self.run.workspace, owner=self.run.user)
        run = AgentRun.objects.create(workspace=self.run.workspace, session=session,
            user=self.run.user, modelConfig=self.run.modelConfig, prompt='independent session', status='running')
        authorization = create_agent_run_authorization(run, image_digest='sha256:' + 'a' * 64)
        job = 'agent_run.lifecycle:' + run.pk
        with connection.cursor() as cursor:
            cursor.execute('INSERT INTO runtime.runtime_jobs(job_id,job_kind,status,lease_owner,lease_expires_at_ms,'
                'idempotency_key,session_id,payload_ref) VALUES(%s,\'agent_run.lifecycle\',\'running\',%s,'
                '(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint+60000,%s,%s,%s)',
                [job, self.owner, job + ':' + authorization.digest, session.pk, 'record:agent_run:' + run.pk])
        def remove_job():
            with connection.cursor() as cursor:
                cursor.execute('DELETE FROM runtime.runtime_jobs WHERE job_id=%s', [job])
        self.addCleanup(remove_job)
        self.assertEqual(run.membership_ref, self.run.membership_ref)
        statements = [(f'SELECT {key} FROM {table} WHERE {key}=%s FOR SHARE', [identity])
            for _, table, key, identity in self.lock_stages()]
        response = self.while_locked(statements, lambda: self.post(run=run, authorization=authorization))
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json()['disposition'], 'ready')
        self.assertEqual(response.json()['agentRunStart']['environmentAuthorization']['payload']['controllerEpoch'], 1)
        self.assertEqual(self.state()[0]['controller_epoch'], 0)

    def test_resolve_control_and_release_preserve_exact_busy_classification(self):
        job_lock = [('SELECT job_id FROM runtime.runtime_jobs WHERE job_id=%s FOR UPDATE', [self.job_id])]
        before = self.state()
        response = self.while_locked(job_lock, lambda: self.post())
        self.assertEqual((response.status_code, response.json()), (409, {'error': 'environment_controller_busy'}))
        self.assertEqual(self.state(), before)
        with connection.cursor() as cursor:
            cursor.execute("UPDATE runtime.runtime_jobs SET lease_purpose='control' WHERE job_id=%s", [self.job_id])
        response = self.while_locked(job_lock, lambda: self.post('control'))
        self.assertEqual((response.status_code, response.json()), (409, {'error': 'environment_controller_busy'}))
        self.assertEqual(self.state(), before)
        with connection.cursor() as cursor:
            cursor.execute("UPDATE runtime.runtime_jobs SET lease_purpose='execute' WHERE job_id=%s", [self.job_id])
        authority = self.cleanup_authority()
        before = self.state()
        response = self.while_locked(job_lock, lambda: self.post('release',
            controllerEpoch=authority['controllerEpoch'], cleanupReceiptId='cleanup:receipt'))
        self.assertEqual((response.status_code, response.json()), (409, {'error': 'environment_controller_busy'}))
        self.assertEqual(self.state(), before)

    def test_receipt_transport_failure_and_invalid_control_lease_keep_their_classification(self):
        before = self.state()
        response = self.post('control')
        self.assertEqual((response.status_code, response.json()),
            (409, {'error': 'agent_run_control_rejected'}))
        self.assertEqual(self.state(), before)
        authority = self.cleanup_authority()
        before = self.state()
        with patch('app_core.http.internal.get_agent_run_cleanup_receipt', side_effect=TimeoutError('receipt transport')):
            response = self.post('release', controllerEpoch=authority['controllerEpoch'], cleanupReceiptId='cleanup:receipt')
        self.assertEqual((response.status_code, response.json()),
            (503, {'error': 'agent_run_cleanup_receipt_unavailable'}))
        self.assertEqual(self.state(), before)

    def test_projection_session_and_run_contention_return_busy_without_mutation(self):
        AgentRun.objects.filter(pk=self.run.pk).update(preAdmissionCancelledAt=timezone.now())
        before = self.state()
        for table, identity in [('app_core_session', self.session.pk), ('app_core_agentrun', self.run.pk)]:
            for path in ['resolve', 'release']:
                with self.subTest(table=table, path=path):
                    fields = {'controllerEpoch': 1, 'cleanupReceiptId': 'cleanup:receipt'} if path == 'release' else {}
                    response = self.while_locked([(f'SELECT id FROM {table} WHERE id=%s FOR SHARE', [identity])],
                        lambda: self.post(path, **fields))
                    self.assertEqual((response.status_code, response.json()),
                        (409, {'error': 'environment_controller_busy'}))
                    self.run.refresh_from_db()
                    self.assertEqual(self.run.status, 'running')
                    self.assertIsNone(self.run.completedAt)
                    self.assertEqual(self.state(), before)
        response = self.post()
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json()['terminalState'], 'cancelled')

    def test_late_projection_contention_rolls_back_the_complete_projection(self):
        from .http.internal import project_committed_agent_run
        AgentRun.objects.filter(pk=self.run.pk).update(preAdmissionCancelledAt=timezone.now())
        before = self.state()
        def project_then_contend(*args, **kwargs):
            project_committed_agent_run(*args, **kwargs)
            with connection.cursor() as cursor:
                cursor.execute('SELECT job_id FROM runtime.runtime_jobs WHERE job_id=%s FOR UPDATE NOWAIT', [self.job_id])
        with patch('app_core.http.internal.project_committed_agent_run', side_effect=project_then_contend):
            response = self.while_locked(
                [('SELECT job_id FROM runtime.runtime_jobs WHERE job_id=%s FOR UPDATE', [self.job_id])], self.post)
        self.assertEqual((response.status_code, response.json()), (409, {'error': 'environment_controller_busy'}))
        self.run.refresh_from_db()
        self.assertEqual(self.run.status, 'running')
        self.assertIsNone(self.run.completedAt)
        self.assertEqual(self.state(), before)

    def test_non_contention_database_errors_propagate_and_roll_back(self):
        from .http.internal import _retained_run_projection, project_committed_agent_run
        before = self.state()
        def database_failure(*args, **kwargs):
            with connection.cursor() as cursor:
                cursor.execute('SELECT 1 / 0')
        with patch('app_core.environment_lifecycle.require_durable_commit', side_effect=database_failure):
            with self.assertRaises(DatabaseError) as raised:
                acquire_environment_controller(self.run.pk, self.owner)
        self.assertEqual(raised.exception.__cause__.sqlstate, '22012')
        self.assertEqual(self.state(), before)
        AgentRun.objects.filter(pk=self.run.pk).update(preAdmissionCancelledAt=timezone.now())
        def project_then_fail(*args, **kwargs):
            project_committed_agent_run(*args, **kwargs)
            database_failure()
        with patch('app_core.http.internal.project_committed_agent_run', side_effect=project_then_fail):
            with self.assertRaises(DatabaseError) as raised:
                _retained_run_projection(self.run.pk, self.authorization.digest)
            self.assertEqual(raised.exception.__cause__.sqlstate, '22012')
            response = self.post()
        self.assertEqual((response.status_code, response.json()), (500, {'error': 'internal_error'}))
        self.run.refresh_from_db()
        self.assertEqual(self.run.status, 'running')
        self.assertIsNone(self.run.completedAt)
        self.assertEqual(self.state(), before)

    def test_shared_authorization_blocks_non_key_status_and_role_updates(self):
        from .environment_lifecycle import _authority
        locked, release = Event(), Event()
        def pause_authority(environment):
            locked.set()
            if not release.wait(10):
                raise AssertionError('test did not release controller transaction')
            return _authority(environment)
        with patch('app_core.environment_lifecycle._authority', side_effect=pause_authority):
            with ThreadPoolExecutor(max_workers=1) as pool:
                contender = pool.submit(self.independently,
                    lambda: acquire_environment_controller(self.run.pk, self.owner))
                try:
                    self.assertTrue(locked.wait(5))
                    for model, identity, changes in [
                        (Workspace, self.run.workspace_id, {'status': 'inactive'}),
                        (WorkspaceMembership, self.run.membership_ref, {'role': 'admin'}),
                    ]:
                        with self.subTest(model=model.__name__):
                            with self.assertRaises(DatabaseError) as raised:
                                with transaction.atomic(), connection.cursor() as cursor:
                                    cursor.execute("SET LOCAL lock_timeout='200ms'")
                                    model.objects.filter(pk=identity).update(**changes)
                            self.assertEqual(raised.exception.__cause__.sqlstate, '55P03')
                finally:
                    release.set()
                    contender.result(timeout=10)
        Workspace.objects.filter(pk=self.run.workspace_id).update(status='inactive')
        with self.assertRaisesRegex(ValueError, 'environment_controller_membership_invalid'):
            acquire_environment_controller(self.run.pk, self.owner)
