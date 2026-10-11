"""Real Core -> Runtime provider -> API validation -> fenced Session commit."""
import json
import os
from pathlib import Path
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor
import threading
import tempfile
from datetime import timedelta
from unittest.mock import patch

from django.conf import settings
from django.contrib.auth import get_user_model
from django.db import connection
from django.test import LiveServerTestCase
from django.utils import timezone

from .agent_run_authorization_factory import create_agent_run_authorization
from .models import Agent, AgentCoordinationSession, AgentDefinition, AgentDefinitionVersion, AgentRun, AgentWorkSession, BusinessApplication, HostedOperationReceipt, ModelConfig, Session, UserAppDelegation, Workspace, WorkspaceMembership
from .runtime_client import build_agent_run_start


class AgentWorkRuntimeTests(LiveServerTestCase):
    serialized_rollback = True

    def test_production_registration_exposes_dispatch_only_to_native_private_coordination(self):
        from .agent_messages import _digest
        user = get_user_model().objects.create_user(username="registration-owner")
        workspace = Workspace.objects.create(name="Registration", createdBy=user)
        membership = WorkspaceMembership.objects.create(workspace=workspace, user=user, role="owner")
        model = ModelConfig.objects.create(displayName="Registration model")
        cases = []
        for kind in ("ordinary", "managed", "delegated", "native"):
            definition = version = None
            fields = {}
            if kind in {"managed", "delegated"}:
                definition = AgentDefinition.objects.create(workspace=workspace, created_by=user,
                    name=kind, availability_scope="workspace")
                version = AgentDefinitionVersion.objects.create(definition=definition, version=1,
                    name=kind, published_by=user)
                definition.published_version = version
                definition.save(update_fields=["published_version"])
                fields["definition_version"] = version
            agent = Agent.objects.create(workspace=workspace, owner=user, name=kind, definition=definition)
            session = Session.objects.create(workspace=workspace, owner=user, agent=agent)
            if kind != "ordinary":
                AgentCoordinationSession.objects.create(agent=agent, session=session)
            if kind == "delegated":
                app = BusinessApplication.objects.create(name="Registration app", created_by=user, status="active")
                grant = UserAppDelegation.objects.create(user=user, app=app, workspace=workspace,
                    definition=definition, membership_ref=membership.id,
                    scopes=["assistant:use", "messages:submit", "sessions:read"],
                    token_digest="sha256:" + "b" * 64, expires_at=timezone.now() + timedelta(hours=1))
                fields.update(acting_app=app, app_delegation=grant)
            run = AgentRun.objects.create(workspace=workspace, session=session, user=user,
                modelConfig=model, prompt="Capture registered tools and finish", status="running", **fields)
            create_agent_run_authorization(run, image_digest="sha256:" + "a" * 64)
            cases.append({"kind": kind, "agentRunStart": build_agent_run_start(run)})
        fixture = {"liveServerUrl": self.live_server_url, "internalToken": settings.INTERNAL_API_TOKEN,
            "signingKey": settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY, "cases": cases,
            "queryContractDigest": _digest("centaeris.dynamic_tool_contract.v1", json.loads(
                (Path(__file__).parent / "contracts/get_work_request.json").read_text(encoding="utf-8")))}
        with tempfile.TemporaryDirectory(prefix="registration-runtime-") as runtime_root:
            fixture["runtimeStoreRoot"] = runtime_root
            result = subprocess.run(["cargo", "test", "--locked", "-p", "runtime_server",
                "agent_work::tests::runtime::django_production_registration_model_tools", "--", "--ignored", "--exact", "--nocapture"],
                cwd=Path(__file__).resolve().parents[3],
                env={**os.environ, "CARGO_BUILD_JOBS": "1", "CENTAERIS_AGENT_REGISTRATION_FIXTURE": json.dumps(fixture),
                     "NO_PROXY": "127.0.0.1,localhost," + os.environ.get("NO_PROXY", "")},
                capture_output=True, text=True, encoding="utf-8", timeout=300)
        diagnostic = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, diagnostic)
        self.assertIn("running 1 test", diagnostic)
        self.assertIn("production-registration-model-tools-ok", diagnostic)
        captured = [json.loads(line.removeprefix("registered model tools: "))
                    for line in diagnostic.splitlines() if line.startswith("registered model tools: ")]
        self.assertEqual([case["kind"] for case in captured], ["ordinary", "managed", "delegated", "native"])
        for case in captured:
            self.assertEqual("dispatch_work" in case["tools"], case["kind"] == "native", case)
            self.assertEqual("get_work_request" in case["tools"], case["kind"] == "native", case)
            self.assertEqual("confirm_work_return" in case["tools"], case["kind"] == "native", case)
        self.assertFalse(AgentWorkSession.objects.exists())
        self.assertFalse(HostedOperationReceipt.objects.exists())

    def invoke(self, fail_commit, fast_trigger_case=None):
        user = get_user_model().objects.create_user(username="dispatch-runtime-owner")
        workspace = Workspace.objects.create(name="Dispatch runtime", createdBy=user)
        WorkspaceMembership.objects.create(workspace=workspace, user=user, role="owner")
        agent = Agent.objects.create(workspace=workspace, owner=user, name="Native coordinator")
        source = Session.objects.create(workspace=workspace, owner=user, agent=agent, origin="automation")
        AgentCoordinationSession.objects.create(agent=agent, session=source)
        model = ModelConfig.objects.create(displayName="Dispatch runtime model")
        run = AgentRun.objects.create(workspace=workspace, session=source, user=user, modelConfig=model,
                                      prompt="Validate work and continue", status="running")
        create_agent_run_authorization(run, image_digest="sha256:" + "a" * 64)
        tables_query = "SELECT tablename FROM pg_tables WHERE schemaname='runtime'"
        with connection.cursor() as cursor:
            cursor.execute("SELECT EXISTS(SELECT 1 FROM pg_namespace WHERE nspname='runtime')")
            previous_schema_exists = cursor.fetchone()[0]
            cursor.execute(tables_query)
            previous_tables = {row[0] for row in cursor.fetchall()}

        def cleanup_runtime_tables():
            with connection.cursor() as cursor:
                cursor.execute(tables_query)
                created = {row[0] for row in cursor.fetchall()} - previous_tables
                if created:
                    names = ", ".join("runtime." + connection.ops.quote_name(name) for name in sorted(created))
                    cursor.execute("DROP TABLE " + names)
                if not previous_schema_exists:
                    cursor.execute("DROP FUNCTION IF EXISTS runtime.notify_runtime_job_ready_v1()")
                    cursor.execute("DROP SCHEMA IF EXISTS runtime")
        self.addCleanup(cleanup_runtime_tables)
        database = settings.DATABASES["default"]
        fixture = {"liveServerUrl": self.live_server_url, "agentRunStart": build_agent_run_start(run),
            "internalToken": settings.INTERNAL_API_TOKEN, "failCommit": fail_commit,
            "fastTriggerCase": fast_trigger_case,
            "database": {key: database[key] for key in ("NAME", "USER", "PASSWORD", "HOST", "PORT")}}
        from .test_agent_work import PROFILE
        # Return-job transport has its own real HTTP/store acceptance. Keep it
        # outside this fixture's controlled admission-response loss window.
        with patch("app_core.agent_work.request_execution_profile", return_value=PROFILE), \
             patch("app_core.agent_work_return_jobs.schedule_work_return_job", return_value="inserted"), \
             patch("app_core.agent_work.schedule_agent_run_lifecycle", return_value="inserted") as schedule:
            result = subprocess.run(["cargo", "test", "--locked", "-p", "runtime_server",
                "agent_work::tests::runtime::django_work_commit_boundary", "--", "--ignored", "--exact", "--nocapture"],
                cwd=Path(__file__).resolve().parents[3],
                env={**os.environ, "CARGO_BUILD_JOBS": "1", "CENTAERIS_AGENT_WORK_FIXTURE": json.dumps(fixture),
                     "NO_PROXY": "127.0.0.1,localhost," + os.environ.get("NO_PROXY", "")},
                capture_output=True, text=True, encoding="utf-8", timeout=300)
        diagnostic = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, diagnostic)
        self.assertIn("running 1 test", diagnostic)
        self.assertIn("work-provider-core-commit-boundary-ok", diagnostic)
        if fast_trigger_case:
            self.assertIn("work-fast-trigger-core-ok: " + fast_trigger_case, diagnostic)
        self.assertEqual(source.events.filter(payload__type="tool_result").exists(),
                         not fail_commit and fast_trigger_case != "fence_failure")
        admitted = fast_trigger_case in {"success", "duplicate", "response_lost"}
        self.assertEqual(Session.objects.count(), 1 + int(admitted))
        self.assertEqual(AgentRun.objects.count(), 1 + int(admitted))
        self.assertEqual(AgentWorkSession.objects.count(), int(admitted))
        self.assertEqual(HostedOperationReceipt.objects.count(), int(admitted))
        self.assertEqual(schedule.call_count, int(admitted))

    def test_provider_success_commits_via_core_continue_turn_without_accepting_work(self):
        self.invoke(False)

    def test_provider_success_followed_by_real_source_commit_failure_creates_no_work(self):
        self.invoke(True)

    def test_committed_source_triggers_one_real_admission_after_core_continues(self):
        self.invoke(False, "success")

    def test_duplicate_commit_wake_reuses_one_real_admission(self):
        self.invoke(False, "duplicate")

    def test_lost_response_and_replayed_commit_reuse_one_real_admission(self):
        self.invoke(False, "response_lost")

    def test_materializer_error_does_not_block_core_final(self):
        self.invoke(False, "api_error")

    def test_materializer_timeout_does_not_block_core_final(self):
        self.invoke(False, "timeout")

    def test_full_trigger_capacity_does_not_block_core_final(self):
        self.invoke(False, "capacity")

    def test_failed_source_commit_never_triggers_materialization(self):
        self.invoke(True, "commit_failure")

    def test_rejected_source_lease_fence_never_triggers_materialization(self):
        self.invoke(False, "fence_failure")

    def run_recovery_worker(self):
        result = subprocess.run([sys.executable,"-B","-c",
            "import worker; worker.WorkRequestReconciler()(); print('work-recovery-live-api-ok')"],
            cwd=Path(__file__).resolve().parents[2]/"worker",
            env={**os.environ,"API_INTERNAL_URL":self.live_server_url,"RUNTIME_INTERNAL_URL":"http://127.0.0.1:1",
                 "INTERNAL_API_TOKEN":settings.INTERNAL_API_TOKEN,"NO_PROXY":"127.0.0.1,localhost"},
            capture_output=True,text=True,encoding="utf-8",timeout=20)
        self.assertEqual(result.returncode,0,result.stdout+result.stderr)
        self.assertIn("work-recovery-live-api-ok",result.stdout)
        print("work-recovery-live-api-ok")

    def recover_with_worker(self):
        from .test_agent_work import PROFILE
        with patch("app_core.agent_work.request_execution_profile", return_value=PROFILE), \
             patch("app_core.agent_work_return_jobs.schedule_work_return_job", return_value="inserted"), \
             patch("app_core.agent_work.schedule_agent_run_lifecycle", return_value="inserted") as schedule:
            self.run_recovery_worker()
        return schedule.call_count

    def test_real_core_capacity_skip_is_recovered_by_real_worker_api_client(self):
        self.invoke(False,"capacity")
        self.assertEqual(self.recover_with_worker(),1)
        operation = HostedOperationReceipt.objects.get()
        self.assertEqual(self.recover_with_worker(),0)
        self.assertEqual(HostedOperationReceipt.objects.get().id,operation.id)
        self.assertEqual((AgentRun.objects.count(),AgentWorkSession.objects.count()),(2,1))

    def test_real_core_lost_response_recovery_does_not_recreate_admission(self):
        self.invoke(False,"response_lost")
        operation = HostedOperationReceipt.objects.get()
        self.assertEqual(self.recover_with_worker(),0)
        self.assertEqual(HostedOperationReceipt.objects.get().id,operation.id)

    def test_two_real_recovery_workers_race_one_real_core_source_admission(self):
        from .test_agent_work import PROFILE
        self.invoke(False,"capacity")
        barrier=threading.Barrier(2)
        with patch("app_core.agent_work.request_execution_profile",side_effect=lambda:(barrier.wait(4),PROFILE)[1]), \
             patch("app_core.agent_work.schedule_agent_run_lifecycle",return_value="inserted") as schedule:
            with ThreadPoolExecutor(max_workers=2) as pool:
                futures=[pool.submit(self.run_recovery_worker) for _ in range(2)]
                for future in futures: future.result(timeout=20)
        self.assertEqual(schedule.call_count,1)
        self.assertEqual((HostedOperationReceipt.objects.count(),AgentWorkSession.objects.count(),AgentRun.objects.count()),(1,1,2))
