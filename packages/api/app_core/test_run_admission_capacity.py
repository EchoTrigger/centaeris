"""Independent Run submissions retain identity without local concurrency quotas."""
from django.test import TransactionTestCase
from django.utils import timezone
from . import models, test_hosted_operations


class ApiRunAdmissionTests(test_hosted_operations.OperationFixture, TransactionTestCase):
    serialized_rollback = True

    def test_independent_submissions_accept_while_other_runs_are_unfinished(self):
        accepted=[]
        for index in range(129):
            response=self.post(operationId=f"independent-{index}")
            self.assertEqual(response.status_code,202,response.content)
            accepted.append(response.json())
        self.assertEqual(len({r['agentRunId'] for r in accepted}),129)
        self.assertEqual(len({r['sessionId'] for r in accepted}),129)
        self.assertEqual(models.AgentRun.objects.filter(status='queued').count(),129)
        self.assertEqual(models.HostedOperationReceipt.objects.filter(command='submitMessage').count(),129)

    def test_running_and_terminal_history_preserve_accepted_replay(self):
        first=self.post()
        self.assertEqual(first.status_code,202,first.content)
        run=models.AgentRun.objects.get(pk=first.json()['agentRunId'])
        for status in ('running','completed'):
            models.AgentRun.objects.filter(pk=run.pk).update(status=status,startedAt=timezone.now(),
                completedAt=timezone.now() if status=='completed' else None)
            self.profile.side_effect=AssertionError('accepted replay must not contact Runtime')
            replay=self.post()
            self.assertEqual(replay.status_code,202,replay.content)
            self.assertEqual(replay.json(),first.json())
        self.assertEqual(models.AgentRun.objects.count(),1)
