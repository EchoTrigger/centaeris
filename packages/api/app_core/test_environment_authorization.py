import copy
import json
from pathlib import Path

from django.test import SimpleTestCase, TestCase

from .test_hosted_operations import OperationFixture


class EnvironmentAuthorizationContractTests(SimpleTestCase):
    def test_signature_matches_shared_vector_and_rejects_unknown_fields(self):
        from .environment_authorization import sign_environment_authorization
        vector = json.loads((Path(__file__).resolve().parents[3] /
            "tests/workspace/fixtures/environment_authorization/v1/valid.json").read_text())
        self.assertEqual(sign_environment_authorization(vector["payload"], "test-key"), vector["signature"])
        for change in ({"executionMode": "persistent"}, {"schema": "unknown"},
                       {"environmentId": ""}, {"instanceId": "caller-selected"}):
            with self.subTest(change=change), self.assertRaises(ValueError):
                sign_environment_authorization(vector["payload"] | change, "test-key")
        with self.assertRaises(ValueError):
            sign_environment_authorization(vector["payload"], "")
        for field in vector["payload"]:
            missing = dict(vector["payload"])
            del missing[field]
            with self.subTest(missing=field), self.assertRaises(ValueError):
                sign_environment_authorization(missing, "test-key")


class EnvironmentAuthorizationAdmissionTests(OperationFixture, TestCase):
    def test_start_binds_the_fixed_environment_without_rewriting_original_grant(self):
        from .models import AgentRun
        from .runtime_client import build_agent_run_start
        from django.db import connection
        from django.test.utils import CaptureQueriesContext
        result = self.post().json()
        run = AgentRun.objects.get(pk=result["agentRunId"])
        original = copy.deepcopy((run.authorization.payload, run.authorization.digest, run.authorization.signature))
        first = build_agent_run_start(run)
        self.assertEqual(first["schema"], "workspace.agent_run.start.v3")
        binding = first["environmentAuthorization"]["payload"]
        self.assertEqual(binding, {"schema": "workspace.environment_authorization.v1",
            "environmentId": run.environment_id, "sessionId": run.session_id,
            "agentRunId": run.pk, "agentRunAuthorizationDigest": run.authorization.digest,
            "executionMode": "perRun"})
        with CaptureQueriesContext(connection) as queries:
            self.assertEqual(build_agent_run_start(run), first)
        self.assertTrue(queries)
        self.assertTrue(all(query["sql"].lstrip().upper().startswith("SELECT") for query in queries),
                        "rebuilding an execution authorization must remain read-only")
        run.authorization.refresh_from_db()
        self.assertEqual((run.authorization.payload, run.authorization.digest, run.authorization.signature), original)
        foreign = AgentRun.objects.get(pk=self.post(operationId="foreign-submit").json()["agentRunId"])
        run.environment = foreign.environment
        with self.assertRaises((ValueError, RuntimeError)):
            build_agent_run_start(run)
