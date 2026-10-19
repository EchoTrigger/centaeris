"""Actual Core native input factory and fenced PostgreSQL writer for admission."""
import json
import os
from pathlib import Path
import subprocess
from unittest.mock import patch
from urllib.parse import quote

from django.conf import settings
from django.db import connection
from django.test import TransactionTestCase

from . import test_agent_work as work_fixture
from . import test_agent_work_returns as return_fixture
from . import test_agent_work_consumption as consume_fixture
from .models import AgentRun
from .testing import fenced_start_fixture
from .runtime_schema_test_helpers import isolate_runtime_schema


class NativeConsumptionRuntimeTests(TransactionTestCase):
    serialized_rollback = True
    dependencies = work_fixture.AgentWorkTests.dependencies
    request_fact = work_fixture.AgentWorkTests.request_fact
    post = work_fixture.AgentWorkTests.post
    child = return_fixture.AgentWorkReturnTests.child
    terminal = return_fixture.AgentWorkReturnTests.terminal
    publish = return_fixture.AgentWorkReturnTests.publish
    notice = consume_fixture.AgentWorkConsumptionTests.notice
    consume = consume_fixture.AgentWorkConsumptionTests.consume

    def setUp(self):
        consume_fixture.AgentWorkConsumptionTests.setUp(self)
        isolate_runtime_schema(self)

    def invoke(self, name, variable, fixture):
        result = subprocess.run(["cargo", "test", "--locked", "-p", "runtime_server",
            "native_consumption_tests::" + name, "--", "--ignored", "--exact", "--nocapture"],
            cwd=Path(__file__).resolve().parents[3], env={**os.environ, variable: json.dumps(fixture)},
            capture_output=True, text=True, encoding="utf-8", timeout=300)
        diagnostic = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, diagnostic)
        return diagnostic

    def test_real_core_native_identity_binds_one_fenced_initial_record_and_reopens(self):
        notice = self.notice()
        def create_input(session_id, input_id, source, content):
            diagnostic = self.invoke("create_native_input", "CENTAERIS_NATIVE_INPUT_REQUEST", {
                "schema":"runtime.host_event_input.create.v1", "sessionId":session_id,
                "inputId":input_id,"source":source,"content":content})
            return json.loads(next(line.removeprefix("core-native-input:") for line in diagnostic.splitlines()
                                   if line.startswith("core-native-input:")))["input"]
        with patch("app_core.agent_work_consumption.request_execution_profile", return_value=work_fixture.PROFILE), \
             patch("app_core.agent_work_consumption.request_host_event_input", side_effect=create_input), \
             patch("app_core.agent_work_consumption.schedule_agent_run_lifecycle", return_value="inserted"):
            response = self.consume(notice)
        self.assertEqual(response.status_code, 201, response.content)
        run = AgentRun.objects.get(pk=response.json()["operation"]["agentRunId"])
        config = connection.settings_dict
        url="postgresql://{}:{}@{}:{}/{}".format(quote(config["USER"]),quote(config["PASSWORD"]),
            config["HOST"],config["PORT"],quote(config["NAME"]))
        diagnostic=self.invoke("native_consumption_fenced_record", "CENTAERIS_NATIVE_CONSUME_FIXTURE", {
            "agentRunStart":fenced_start_fixture(run),"databaseUrl":url,"signingKey":settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY})
        self.assertIn("native-consume-fenced-record-ok:",diagnostic)
        self.assertEqual(list(run.events.order_by("agent_run_sequence").values_list("payload__type",flat=True)),
                         ["agent_run_started","host_event_input"])
