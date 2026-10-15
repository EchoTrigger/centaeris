import json
from unittest.mock import patch

from django.db import connection
from django.test import TransactionTestCase
from django.utils import timezone

from .models import AgentRun, SessionEvent, Workspace
from .session_event import committed_session_terminal_state, project_committed_agent_run
from . import test_retained_lifecycle as lifecycle
from .test_execution_publication_fencing import ExecutionPublicationFixture


class PreAdmissionFailureTests(ExecutionPublicationFixture, TransactionTestCase):
    serialized_rollback = True
    post = lifecycle.RetainedLifecycleTests.post

    def setUp(self):
        super().setUp()
        self.at_ms = int(timezone.now().timestamp() * 1000)
        self.event_id = 'agent_run_pre_admission_failure:' + self.run.id
        self.payload = {'schema': 'runtime.agent_run.pre_admission_failure.v1',
            'workspaceId': self.run.workspace_id, 'sessionId': self.run.session_id,
            'agentRunId': self.run.id, 'authorizationDigest': self.authorization.digest,
            'jobId': self.job_id, 'leaseOwner': self.owner, 'failedAtMs': self.at_ms}
        with connection.cursor() as cursor:
            cursor.execute('CREATE TABLE IF NOT EXISTS runtime.runtime_events ('
                'event_id text PRIMARY KEY, session_id text NOT NULL, task_id text, '
                'event_type text NOT NULL, at_ms bigint NOT NULL, visibility text NOT NULL, payload_json text NOT NULL)')
        self.addCleanup(self.remove_receipt)

    def remove_receipt(self):
        with connection.cursor() as cursor:
            cursor.execute('DELETE FROM runtime.runtime_events WHERE event_id=%s', [self.event_id])

    def receipt(self, **changes):
        payload = self.payload | changes
        self.remove_receipt()
        receipt_ms = int(payload["failedAtMs"]) if type(payload["failedAtMs"]) in (int, bool) else self.at_ms
        with connection.cursor() as cursor:
            cursor.execute('INSERT INTO runtime.runtime_events VALUES (%s,%s,%s,%s,%s,%s,%s)',
                [self.event_id, self.run.session_id, self.run.id, self.payload['schema'], receipt_ms,
                 'internal', json.dumps(payload)])

    def test_receipt_projects_failure_without_model_history_and_resolves_cleanup(self):
        self.receipt()
        self.assertEqual(committed_session_terminal_state(self.run), 'failed')
        result = self.post()
        self.assertEqual(result.status_code, 200, result.content)
        self.assertEqual(result.json()['terminalState'], 'failed')
        self.assertEqual(result.json()['agentRunCleanup']['environmentAuthorization']['payload']['controllerPurpose'], 'cleanup')
        self.run.refresh_from_db()
        self.assertEqual(self.run.status, 'failed')
        self.assertIsNone(self.run.startedAt)
        self.assertEqual(int(self.run.completedAt.timestamp()*1000), self.at_ms)
        self.assertFalse(SessionEvent.objects.filter(agent_run=self.run).exists())

    def test_cancellation_projection_preserves_committed_timestamp_precision(self):
        stamp = timezone.now().replace(microsecond=123456)
        AgentRun.objects.filter(pk=self.run.pk).update(preAdmissionCancelledAt=stamp)
        projected = project_committed_agent_run(self.run, 'cancelled')
        self.assertEqual(projected.status, 'cancelled')
        self.assertEqual(projected.completedAt, stamp)

    def test_status_only_is_not_failure_evidence(self):
        AgentRun.objects.filter(pk=self.run.pk).update(status='failed')
        self.assertIsNone(committed_session_terminal_state(self.run))
        response = self.post()
        self.assertEqual(response.status_code, 409)
        self.assertEqual(response.json()['error'], 'agent_run_semantic_terminal_missing')

    def test_receipt_provenance_survives_lease_replacement(self):
        self.receipt()
        self._lease(self.replacement)
        self.assertEqual(committed_session_terminal_state(self.run), 'failed')

    def test_receipt_rejects_mismatched_or_unsupported_payload(self):
        changes = [{'workspaceId': 'other'}, {'sessionId': 'other'}, {'agentRunId': 'other'},
            {'authorizationDigest': 'sha256:'+'b'*64}, {'jobId': 'other'}, {'leaseOwner': ''}, {'leaseOwner': '   '},
            {'failedAtMs': True}, {'failedAtMs': 0}, {'failedAtMs': -1}, {'failedAtMs': 253402300800000},
            {'schema': 'runtime.agent_run.pre_admission_failure.v2'}, {'unknown': 'field'}]
        for change in changes:
            with self.subTest(fields=list(change)):
                self.receipt(**change)
                with self.assertRaises(ValueError):
                    committed_session_terminal_state(self.run)

    def test_receipt_rejects_row_binding_and_time_mismatch(self):
        for column, value in [('session_id','other'), ('task_id','other'), ('event_type','other'),
                ('visibility','public'), ('at_ms', self.at_ms + 1), ('payload_json', '[]')]:
            with self.subTest(column=column):
                self.receipt()
                with connection.cursor() as cursor:
                    cursor.execute(f'UPDATE runtime.runtime_events SET {column}=%s WHERE event_id=%s', [value, self.event_id])
                with self.assertRaises(ValueError):
                    committed_session_terminal_state(self.run)

    def test_failure_conflicts_with_cancellation_or_admitted_history(self):
        self.receipt()
        self.run.preAdmissionCancelledAt = timezone.now()
        with self.assertRaises(ValueError):
            committed_session_terminal_state(self.run)
        self.run.preAdmissionCancelledAt = None
        SessionEvent.objects.create(eventId='admitted', workspace=self.run.workspace, session=self.session,
            agent_run=self.run, sequence=1, agent_run_sequence=1, createdAtMs=self.at_ms,
            projects_to_agent_run_stream=True,
            payload={'type':'agent_run_started', 'createdAtMs':self.at_ms, 'payload':{}})
        with self.assertRaises(ValueError):
            committed_session_terminal_state(self.run)
        with self.assertRaises(ValueError):
            project_committed_agent_run(self.run)

        other = Workspace.objects.create(name='Mismatched record', createdBy=self.run.user)
        SessionEvent.objects.filter(agent_run=self.run).update(workspace=other)
        with self.assertRaises(ValueError):
            committed_session_terminal_state(self.run)
        with self.assertRaises(ValueError):
            project_committed_agent_run(self.run)

    def test_projection_refuses_inconsistent_expected_state(self):
        self.receipt()
        with self.assertRaises(ValueError):
            project_committed_agent_run(self.run, 'completed')

    def test_preview_uses_typed_failure_timestamp(self):
        from .http.agent_previews import RunFact, _run_fact
        self.receipt()
        fact = _run_fact(self.session)
        self.assertEqual(fact['sourceType'], 'preAdmissionFailure')
        self.assertEqual(fact['status'], 'failed')
        self.assertEqual(fact['createdAtMs'], self.at_ms)
        RunFact.model_validate(fact)

    def test_work_return_uses_failure_receipt(self):
        from .agent_work_returns import _terminal_fact
        self.receipt()
        fact = _terminal_fact(self.run)
        self.assertEqual(fact, {'kind':'preAdmissionFailure','factRef':self.event_id,
            'state':'failed','eventType':None,'reasonType':'preAdmissionFailure',
            'atMs':self.at_ms,'throughSequence':0})

    def test_missing_postgres_ledger_is_absent_but_database_errors_propagate(self):
        from django.db import DatabaseError
        with patch('app_core.session_event.connection') as local:
            local.vendor = 'postgresql'
            cursor = local.cursor.return_value.__enter__.return_value
            cursor.fetchone.return_value = (None,)
            self.assertIsNone(committed_session_terminal_state(self.run))
            cursor.execute.side_effect = DatabaseError('unavailable')
            with self.assertRaises(DatabaseError):
                committed_session_terminal_state(self.run)

    def test_sqlite_missing_runtime_schema_is_absent(self):
        with patch('app_core.session_event.connection') as local:
            local.vendor = 'sqlite'
            self.assertIsNone(committed_session_terminal_state(self.run))
            local.cursor.assert_not_called()

from . import test_agent_work_returns as work_fixture


class PreAdmissionFailureWorkReturnTests(TransactionTestCase):
    serialized_rollback = True
    setUp = work_fixture.AgentWorkReturnTests.setUp
    dependencies = work_fixture.AgentWorkReturnTests.dependencies
    post = work_fixture.AgentWorkReturnTests.post
    request_fact = work_fixture.AgentWorkReturnTests.request_fact
    child = work_fixture.AgentWorkReturnTests.child
    publish = work_fixture.AgentWorkReturnTests.publish
    query = work_fixture.AgentWorkReturnTests.query

    def test_failure_receipt_publishes_one_queryable_immutable_work_return(self):
        from .models import AgentWorkReturn
        with patch('app_core.agent_work_return_jobs.schedule_work_return_job', return_value='inserted'):
            child = self.child()
        event_id = 'agent_run_pre_admission_failure:' + child.id
        at_ms = int(timezone.now().timestamp()*1000)
        payload = {'schema':'runtime.agent_run.pre_admission_failure.v1',
            'workspaceId':child.workspace_id,'sessionId':child.session_id,'agentRunId':child.id,
            'authorizationDigest':child.authorization.digest,'jobId':'agent_run.lifecycle:'+child.id,
            'leaseOwner':'worker:original-owner','failedAtMs':at_ms}
        with connection.cursor() as cursor:
            cursor.execute('CREATE SCHEMA IF NOT EXISTS runtime')
            cursor.execute('CREATE TABLE IF NOT EXISTS runtime.runtime_events ('
                'event_id text PRIMARY KEY, session_id text NOT NULL, task_id text, '
                'event_type text NOT NULL, at_ms bigint NOT NULL, visibility text NOT NULL, payload_json text NOT NULL)')
            cursor.execute('INSERT INTO runtime.runtime_events VALUES (%s,%s,%s,%s,%s,%s,%s)',
                [event_id,child.session_id,child.id,payload['schema'],at_ms,'internal',json.dumps(payload)])
        def remove():
            with connection.cursor() as cursor:
                cursor.execute('DELETE FROM runtime.runtime_events WHERE event_id=%s',[event_id])
        self.addCleanup(remove)
        response = self.publish(child.id)
        self.assertEqual(response.status_code,201,response.content)
        notice=response.json()['notice']
        self.assertEqual(notice['terminal']['kind'],'preAdmissionFailure')
        self.assertEqual(notice['terminal']['state'],'failed')
        self.assertEqual(notice['terminal']['atMs'],at_ms)
        self.assertEqual(AgentWorkReturn.objects.get().fact_kind,'preAdmissionFailure')
        self.assertEqual(self.publish(child.id).json()['disposition'],'duplicate')
        self.assertEqual(self.query(notice['noticeId']).json()['notice'],notice)
