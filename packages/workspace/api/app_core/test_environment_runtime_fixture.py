"""Owned ordinary Session fixture for the actual Runtime/API lifecycle boundary."""
import importlib.util
import json
import os
from pathlib import Path
import queue
import subprocess
import sys
import threading
import time
from unittest.mock import patch
from urllib.parse import quote
from django.conf import settings
from django.contrib.auth import get_user_model
from django.db import connection
from .agent_run_authorization_factory import create_agent_run_authorization
from .models import AgentRun, ModelConfig, Session, Workspace, WorkspaceMembership
from .runtime_schema_test_helpers import isolate_runtime_schema

class EnvironmentRuntimeFixture:
    def setUp(self):
        super().setUp()
        isolate_runtime_schema(self)
        self.user = get_user_model().objects.create_user(username="environment-runtime-owner")
        self.workspace = Workspace.objects.create(name="Environment runtime", createdBy=self.user)
        WorkspaceMembership.objects.create(workspace=self.workspace, user=self.user, role="owner")
        self.model = ModelConfig.objects.create(displayName="Environment runtime model")
        self.source = Session.objects.create(workspace=self.workspace, owner=self.user)
        self.run = AgentRun.objects.create(workspace=self.workspace, session=self.source,
            user=self.user, modelConfig=self.model, prompt="Complete the environment lifecycle objective.",
            agent_instructions="Finish the requested objective.")
        create_agent_run_authorization(self.run, image_digest="sha256:" + "a" * 64)
        self.requests, self.runtime_lines = [], []
        self.runtime_markers = queue.Queue()

    def start_runtime(self):
        self.runtime_command_lock = threading.Lock()
        self.cleanup_proofs = {}
        database = connection.settings_dict
        url = "postgresql://{}:{}@{}:{}/{}".format(
            quote(database["USER"]), quote(database["PASSWORD"]), database["HOST"],
            database["PORT"], quote(database["NAME"]))
        fixture = {"databaseUrl": url, "userId": str(self.user.pk), "workspaceId": self.workspace.pk,
                   "agentRunId": self.run.pk}
        environment = {**os.environ, "CARGO_BUILD_JOBS": "1",
            "CENTAERIS_ENVIRONMENT_RUNTIME_FIXTURE": json.dumps(fixture),
            "API_INTERNAL_URL": self.live_server_url, "INTERNAL_API_TOKEN": settings.INTERNAL_API_TOKEN,
            "AGENT_RUN_AUTHORIZATION_SIGNING_KEY": settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY,
            "REDIS_URL": "redis://127.0.0.1:1/15", "RUNTIME_STREAM_TTL_SECONDS": "3600",
            "RUNTIME_LIVE_STATE_TTL_SECONDS": "3600",
            "NO_PROXY": "127.0.0.1,localhost," + os.environ.get("NO_PROXY", "")}
        fixture_name = "environment_runtime_tests::environment_runtime_server"
        executable = os.environ.get("CENTAERIS_TEST_RUNTIME_EXECUTABLE")
        command = ([executable, fixture_name] if executable else
                   ["cargo", "test", "--locked", "-p", "runtime_server", fixture_name, "--"])
        self.runtime_process = subprocess.Popen(
            [*command, "--ignored", "--exact", "--nocapture"], cwd=Path(__file__).resolve().parents[4],
            env=environment, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            text=True, encoding="utf-8")
        self.addCleanup(self.stop_runtime)

        def collect():
            for line in self.runtime_process.stdout:
                self.runtime_lines.append(line)
                if line.startswith("environment-runtime-"):
                    self.runtime_markers.put(line.rstrip())
            self.runtime_markers.put("process-exited")

        self.runtime_reader = threading.Thread(target=collect, daemon=True)
        self.runtime_reader.start()
        # Direct Django invocations may still build the fixture on first use.
        ready = self.runtime_markers.get(timeout=900)
        self.assertTrue(ready.startswith("environment-runtime-ready:"), "".join(self.runtime_lines))
        return ready.removeprefix("environment-runtime-ready:")


    def stop_runtime(self):
        with self.runtime_command_lock:
            if self.runtime_process.poll() is None:
                self.runtime_process.stdin.write("stop\n")
                self.runtime_process.stdin.flush()
        self.runtime_process.wait(timeout=30)
        self.runtime_reader.join(timeout=5)
        self.runtime_process.stdin.close()
        self.runtime_process.stdout.close()
        self.assertEqual(self.runtime_process.returncode, 0, "".join(self.runtime_lines))


    def runtime_command(self, command, marker):
        """Keep each stdin request and its proof together on the shared stream."""
        with self.runtime_command_lock:
            self.assertIsNone(self.runtime_process.poll(), "".join(self.runtime_lines))
            self.runtime_process.stdin.write(command + "\n")
            self.runtime_process.stdin.flush()
            proof = self.runtime_markers.get(timeout=10)
            self.assertTrue(proof.startswith(marker), "".join(self.runtime_lines))
            return json.loads(proof.removeprefix(marker))


    def assert_lifecycle_cleanup(self, run):
        proof = self.runtime_command("verify-cleanup:" + run.pk, "environment-runtime-cleanup:")
        self.assertEqual(proof["agentRunId"], run.pk)
        allocations = proof["allocations"]
        self.assertIsInstance(allocations, list)
        self.assertTrue(allocations, "a succeeded lifecycle must prove its actual instance cleanup")
        identities = {name: [] for name in ("instanceId", "executionId", "removedContainerId")}
        for allocation in allocations:
            self.assertEqual(allocation["allocationKind"], "instance")
            for name in ("cleanupComplete", "physicalRemoved", "noResidentCharge", "physicalAbsent"):
                self.assertIs(allocation[name], True, allocation)
            for name, values in identities.items():
                self.assertIsInstance(allocation[name], str)
                self.assertTrue(allocation[name].strip(), allocation)
                values.append(allocation[name])
        for name, values in identities.items():
            self.assertEqual(len(values), len(set(values)), f"duplicate {name}: {proof}")
        for allocation in allocations:
            self.assertRegex(allocation["removedContainerId"], r"^[0-9a-f]{64}$")
        execution_ids = list(run.events.filter(payload__type="agent_run_execution_started").values_list(
            "payload__payload__executionId", flat=True))
        self.assertTrue(execution_ids, "cleanup must match committed execution identities")
        self.assertCountEqual(identities["executionId"], execution_ids)
        for previous in self.cleanup_proofs.values():
            self.assertTrue(set(identities["executionId"]).isdisjoint(
                allocation["executionId"] for allocation in previous["allocations"]), proof)
            self.assertTrue(set(identities["removedContainerId"]).isdisjoint(
                allocation["removedContainerId"] for allocation in previous["allocations"]), proof)
        self.cleanup_proofs[run.pk] = proof
        return proof


    def load_worker(self, runtime_url):
        path = Path(__file__).resolve().parents[2] / "worker"
        with patch.dict(os.environ, {"RUNTIME_INTERNAL_URL": runtime_url,
                "API_INTERNAL_URL": self.live_server_url, "INTERNAL_API_TOKEN": settings.INTERNAL_API_TOKEN,
                # A legal positive recheck retains real Core due-time waiting
                # while keeping this isolated HTTP fixture within five seconds.
                "AGENT_RUN_LIFECYCLE_RECHECK_MS": "1000"}), \
             patch.object(sys, "path", [str(path), *sys.path]):
            spec = importlib.util.spec_from_file_location("environment_runtime_worker", path / "worker.py")
            worker = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(worker)
        return worker


    def claim_due_job(self, worker, job_id, job_kind, worker_id):
        """Observe durable due time under a monotonic deadline before claiming."""
        deadline = time.monotonic() + 5
        def poll_request(path, body=None):
            remaining = deadline - time.monotonic()
            self.assertGreater(remaining, 0, f"job claim deadline exceeded: {job_id}")
            return worker.json_request(f"{worker.RUNTIME_INTERNAL_URL}{path}", body,
                "X-Internal-Token", worker.INTERNAL_API_TOKEN, "native_job_claim_unavailable",
                timeout=remaining)
        while True:
            jobs = poll_request("/internal/jobs/claim", {
                "schema": "runtime.job.claim.v1", "workerId": worker_id,
                "jobId": job_id, "jobKind": job_kind, "nowMs": worker.now_ms(),
                "leaseMs": 300000, "limit": 1})["jobs"]
            if jobs:
                break
            current = poll_request("/internal/jobs/" + job_id)["job"]
            remaining = deadline - time.monotonic()
            self.assertGreater(remaining, 0, f"job did not become due: {current}")
            self.assertEqual((current["jobId"], current["jobKind"]), (job_id, job_kind))
            self.assertEqual(current["status"], "queued", current)
            self.assertIs(type(current["runAtMs"]), int)
            due_seconds = max(0.01, (current["runAtMs"] - worker.now_ms()) / 1000)
            time.sleep(min(due_seconds, remaining))
        self.assertEqual(len(jobs), 1)
        job = jobs[0]
        self.assertEqual((job["jobId"], job["jobKind"], job["status"]), (job_id, job_kind, "leased"))
        current = poll_request("/internal/jobs/" + job_id)["job"]
        self.assertEqual((current["status"], current["leaseOwner"]), ("leased", job["leaseOwner"]))
        return job
