"""Semantic controls do not acquire physical execution authority."""
from unittest.mock import patch

from django.test import TestCase

from .models import AgentRun
from .test_hosted_operations import OperationFixture


class AgentRunControlTests(OperationFixture, TestCase):
    def run_fixture(self):
        result = self.post()
        self.assertEqual(result.status_code, 202, result.content)
        return AgentRun.objects.get(pk=result.json()['agentRunId'])

    def expected(self, run):
        return {'agentRunId': run.pk, 'authorizationDigest': run.authorization.digest,
            'authorizationSignature': run.authorization.signature, 'authorization': run.authorization.payload}

    def test_queued_cancellation_uses_only_the_original_grant_without_a_controller(self):
        from .runtime_client import request_agent_run_cancellation
        run = self.run_fixture()
        with patch('app_core.runtime_client.request_json', return_value={'schema': 'runtime.agent_run.cancel.result.v1',
                'agentRunId': run.pk, 'disposition': 'requested', 'terminalState': None}) as request:
            request_agent_run_cancellation(run)
        self.assertEqual(request.call_args.args[1], {'schema': 'runtime.agent_run.cancel.v1',
            'agentRunControl': self.expected(run)})
        run.environment.refresh_from_db()
        self.assertEqual(run.environment.controller_epoch, 0)
        self.assertIsNone(run.environment.controller_run_id)

    def test_queued_supplement_uses_only_the_original_grant_without_a_controller(self):
        from .runtime_client import request_agent_run_supplement
        run = self.run_fixture()
        result = {'schema': 'runtime.agent_run.supplement.result.v1', 'accepted': True, 'disposition': 'accepted',
            'agentRunId': run.pk, 'sessionId': run.session_id, 'supplementId': 'supplement-one',
            'queuedCount': 1, 'queueRevision': 1}
        with patch('app_core.runtime_client.request_json', return_value=result) as request:
            request_agent_run_supplement(run, 'supplement-one', 'continue')
        self.assertEqual(request.call_args.args[1], {'schema': 'runtime.agent_run.supplement.v1',
            'supplementId': 'supplement-one', 'jobId': f'agent_run.lifecycle:{run.pk}', 'message': 'continue',
            'agentRunControl': self.expected(run)})
        run.environment.refresh_from_db()
        self.assertEqual(run.environment.controller_epoch, 0)
