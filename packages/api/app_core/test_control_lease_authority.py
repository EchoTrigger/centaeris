"""Control lease authority never becomes ordinary Environment execution authority."""
import json
from django.conf import settings
from django.db import connection
from django.test import TransactionTestCase
from .test_execution_publication_fencing import ExecutionPublicationFixture


class ControlLeaseAuthorityTests(ExecutionPublicationFixture, TransactionTestCase):
    serialized_rollback = True

    def control_lease(self):
        with connection.cursor() as cursor:
            cursor.execute("UPDATE runtime.runtime_jobs SET lease_purpose='control' WHERE job_id=%s", [self.job_id])

    def test_control_lease_reads_original_grant_without_acquiring_environment(self):
        from .environment_lifecycle import acquire_environment_controller, read_control_authorization
        self.control_lease()
        with self.assertRaisesRegex(ValueError, 'environment_controller_lease_rejected'):
            acquire_environment_controller(self.run.pk, self.owner)
        result = read_control_authorization(self.run.pk, self.owner)
        self.assertEqual(result['authorizationDigest'], self.authorization.digest)
        self.assertEqual(result['authorization'], self.authorization.payload)
        self.run.environment.refresh_from_db()
        self.assertEqual(self.run.environment.controller_epoch, 0)
        self.assertIsNone(self.run.environment.controller_run_id)

    def request(self, path, schema, **extra):
        return self.client.post(path, data=json.dumps({'schema': schema, 'jobId': self.job_id,
            'leaseOwner': self.owner, 'agentRunId': self.run.pk,
            'authorizationDigest': self.authorization.digest, **extra}), content_type='application/json',
            HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN)

    def test_control_lease_cannot_obtain_agent_run_start(self):
        self.control_lease()
        response = self.request('/internal/agent-run-lifecycle/resolve',
            'runtime.agent_run_lifecycle.resolve.v1', executionMode='perRun')
        self.assertEqual(response.status_code, 409, response.content)
        self.run.environment.refresh_from_db()
        self.assertEqual(self.run.environment.controller_epoch, 0)

    def test_control_resolution_requires_a_current_control_lease(self):
        path='/internal/agent-run-lifecycle/control'
        schema='runtime.agent_run_lifecycle.control.resolve.v1'
        self.assertEqual(self.request(path, schema).status_code, 409)
        self.control_lease()
        result=self.request(path, schema)
        self.assertEqual(result.status_code, 200, result.content)
        self.assertEqual(set(result.json()), {'schema', 'agentRunControl'})
        self.assertNotIn('environmentAuthorization', result.json()['agentRunControl'])

    def test_control_lease_cannot_call_the_model_endpoint(self):
        self.control_lease()
        response=self.client.post('/internal/model-runs',data=json.dumps({
            'schema':'api.model.run.v1','agentRunId':self.run.pk,
            'authorizationRef':self.authorization.pk,'authorizationDigest':self.authorization.digest,
            'modelConfigRef':self.run.modelConfig_id,'thinkingMode':self.run.thinkingMode or None,
            'maxOutputTokens':self.run.modelConfig.maxOutputTokens,
        }),content_type='application/json',HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN)
        self.assertEqual(response.status_code,409,response.content)
        self.assertEqual(response.json(),{'error':'model_execution_lease_required'})
