"""Opt-in automatic rotation through public admission, Worker, Runtime and E2B."""
import json
import os
import shlex
import threading
import time
from copy import deepcopy
from unittest.mock import patch
from urllib.error import HTTPError
from urllib.request import ProxyHandler, Request, build_opener
from uuid import uuid4

from django.conf import settings
from django.db import connection
from django.test import LiveServerTestCase, SimpleTestCase, override_settings

from . import test_e2b_retained_consumption as retained_fixture
from . import test_e2b_power_consumption as power_fixture
from .e2b_power_timing_test_support import PowerTimingCollector, PowerTimingMultiplexer

ADAPTER_PROCESS_PATTERN = "centaeris_e2b_adapter"


def kill_adapter_processes():
    """SIGKILL every live adapter; a lost reply must come from a real transport death."""
    import signal
    killed = 0
    for entry in os.listdir("/proc"):
        if not entry.isdigit():
            continue
        try:
            with open(f"/proc/{entry}/cmdline", "rb") as handle:
                cmdline = handle.read()
        except OSError:
            continue
        if ADAPTER_PROCESS_PATTERN.encode() in cmdline:
            try:
                os.kill(int(entry), signal.SIGKILL)
                killed += 1
            except OSError:
                pass
    return killed
PG_STAT_COLUMNS = ("xact_commit", "xact_rollback", "blks_read", "blks_hit",
    "tup_returned", "tup_fetched", "tup_inserted", "tup_updated", "tup_deleted")
CLOCK_BOUNDARY = ("Power operation timestamps, wake events and Job due times use the database clock; "
    "fixture intervals use the fixture monotonic clock; tool events use the API clock. "
    "Values from different clocks are reported side by side and never subtracted.")
METRIC_BOUNDARY = ("Endpoint and power stage durations contain nested, overlapping spans and "
    "must never be added to construct a total latency.")


def rotation_acceptance_enabled(environment):
    return all(environment.get(name) == "1" for name in (
        "CENTAERIS_E2B_PRODUCT_ACCEPTANCE", "CENTAERIS_E2B_RETAINED_ACCEPTANCE",
        "CENTAERIS_E2B_ROTATION_ACCEPTANCE"))


def require_rotation_target(actual, saved):
    """Compare provider identity independently of the fixture's Session annotation."""
    if actual != {key: value for key, value in saved.items() if key != "sessionId"}:
        raise AssertionError("retained_rotation_target_changed")


def preserved_unknown_creation(read_state):
    row = read_state()
    return row if row == ("dispatched", None, None, None, 1) else None


def rotation_teardown_is_safe(fixture, database, owner):
    """Allow an expected Unknown to retain its whole DB; never authorize flushing it."""
    declared = getattr(fixture, "directed_lost_reply_attempt", None)
    if declared is None:
        return power_fixture.power_teardown_is_safe(fixture, database, owner)
    if not getattr(fixture, "_owner_verified", False):
        return False
    try:
        with database.cursor() as cursor:
            cursor.execute("SELECT current_database(),shobj_description(oid,'pg_database') "
                "FROM pg_database WHERE datname=current_database()")
            retained_fixture.product_fixture.require_owned_database(*cursor.fetchone(), owner)
            cursor.execute("SELECT count(*) FROM runtime.provider_environment_run_uses WHERE cleanup_receipt_id IS NULL")
            if cursor.fetchone()[0] != 0:
                return False
            cursor.execute("SELECT count(*) FROM runtime.provider_environment_power_operations WHERE ack_json IS NULL")
            if cursor.fetchone()[0] != 0:
                return False
            cursor.execute("SELECT attempt_id,dispatch_state,reported_sandbox_id,verified_sandbox_id,"
                "settlement_id FROM runtime.provider_creation_operations WHERE settlement_id IS NULL")
            rows = cursor.fetchall()
        return rows == [(declared, "dispatched", None, None, None)]
    except Exception:
        return False


def rotation_commands(root, marker, visit):
    """The counter lives only in a guest process; files cannot recreate it."""
    server = '''import json,os,socket,sys
s=socket.socket(socket.AF_UNIX); s.bind(sys.argv[1]); s.listen()
n=0
while True:
 c,_=s.accept()
 with c:
  if c.recv(1)==b'+': n+=1
  c.sendall(json.dumps([n,os.getpid(),open('/proc/self/stat').read().split()[21]]).encode())
'''
    source = f'''import json,pathlib,socket,subprocess,sys,time
marker={marker!r}
proof=pathlib.Path({root!r})/'.centaeris-rotation-proof'
identity=pathlib.Path(marker+'.identity')
visit={visit}
if visit==1:
 assert not identity.exists() and not pathlib.Path(marker).exists()
 subprocess.Popen([sys.executable,'-c',{server!r},marker],stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,start_new_session=True)
 for _ in range(100):
  if pathlib.Path(marker).exists(): break
  time.sleep(.02)
else:
 assert proof.read_text()==str(visit-1)
s=socket.socket(socket.AF_UNIX); s.settimeout(5); s.connect(marker); s.sendall(b'+')
value=json.loads(s.recv(4096)); s.close()
assert value[0]==visit, 'retained_memory_counter_changed'
if visit==1: identity.write_text(json.dumps(value[1:]))
else: assert json.loads(identity.read_text())==value[1:], 'retained_process_identity_changed'
proof.write_text(str(visit))
print(json.dumps({{'counter':value[0],'pid':value[1],'startTicks':value[2]}}))
'''
    proof_path = shlex.quote(root + "/.centaeris-rotation-proof")
    return ["python3 - <<'ROTATION_PROOF'\n" + source + "ROTATION_PROOF",
        f"test \"$(cat {proof_path})\" = {visit}",
        "python3 - <<'ROTATION_IDENTITY'\nimport json,pathlib\n"
        + f"pid,ticks=json.loads(pathlib.Path({(marker+'.identity')!r}).read_text())\n"
        + "assert pathlib.Path('/proc/'+str(pid)+'/stat').read_text().split()[21]==ticks\n"
        + "print('same-process-memory')\nROTATION_IDENTITY",
        f"cat {proof_path}"]


class RotationAcceptanceGuardTests(SimpleTestCase):
    def test_discovery_requires_all_explicit_opt_ins(self):
        names = ("CENTAERIS_E2B_PRODUCT_ACCEPTANCE", "CENTAERIS_E2B_RETAINED_ACCEPTANCE",
                 "CENTAERIS_E2B_ROTATION_ACCEPTANCE")
        selected = dict.fromkeys(names, "1")
        for name in names:
            self.assertFalse(rotation_acceptance_enabled(selected | {name: "0"}))
        self.assertFalse(rotation_acceptance_enabled(selected | {names[-1]: "true"}))
        self.assertTrue(rotation_acceptance_enabled(selected))

    def test_repeat_target_accepts_the_saved_session_annotation_only(self):
        actual = {"sandboxId": "sandbox-a", "instanceId": "instance-a", "metadata": {"sessionId": "session-a"}}
        require_rotation_target(actual, dict(actual, sessionId="session-a"))
        with self.assertRaises(AssertionError):
            require_rotation_target(dict(actual, sandboxId="sandbox-b"), dict(actual, sessionId="session-a"))

    def test_unknown_predicate_reads_before_checking_and_rejects_changed_facts(self):
        expected = ("dispatched", None, None, None, 1)
        self.assertEqual(preserved_unknown_creation(lambda: expected), expected)
        for row in (None, ("prepared", None, None, None, 1),
                    ("dispatched", "sandbox", None, None, 1),
                    ("dispatched", None, None, None, 2)):
            with self.subTest(row=row):
                self.assertIsNone(preserved_unknown_creation(lambda: row))

    def database(self, rows, *, pending_uses=0, pending_power=0):
        class Cursor:
            fetchall = staticmethod(lambda: rows)
            values = iter((("test_rotation", "centaeris:e2b-product:owner"), (pending_uses,), (pending_power,)))
            fetchone = lambda self: next(self.values)
            execute = lambda self, *args: None
        class Context:
            def __enter__(self): return Cursor()
            def __exit__(self, *args): return False
        class Database:
            cursor = staticmethod(lambda: Context())
        return Database()

    def fixture_with(self, declared):
        class Fixture: pass
        fixture = Fixture()
        fixture.directed_lost_reply_attempt = declared
        fixture._owner_verified = True
        return fixture

    @patch.object(power_fixture, "power_teardown_is_safe", return_value=True)
    def test_teardown_accepts_only_the_single_declared_terminal_lost_reply_unknown(self, _power):
        unknown = ("attempt", "dispatched", None, None, None)
        for declared, rows, expected in (
            (None, [], True),
            ("attempt", [unknown], True),
            ("attempt", [], False),
            ("attempt", [("other",) + unknown[1:]], False),
            ("attempt", [("attempt", "prepared", None, None, None)], False),
            ("attempt", [("attempt", "dispatched", "reported", None, None)], False),
            ("attempt", [("attempt", "dispatched", None, "verified", None)], False),
            ("attempt", [("attempt", "dispatched", None, None, "settled")], False),
            ("attempt", [unknown, unknown], False),
        ):
            with self.subTest(declared=declared, rows=rows):
                self.assertIs(rotation_teardown_is_safe(self.fixture_with(declared),
                    self.database(rows), "owner"), expected)

    def test_expected_unknown_keeps_ownership_and_all_other_cleanup_requirements(self):
        rows = [("attempt", "dispatched", None, None, None)]
        fixture = self.fixture_with("attempt")
        for fields in ({"pending_uses": 1}, {"pending_power": 1}):
            with self.subTest(fields=fields):
                self.assertFalse(rotation_teardown_is_safe(fixture, self.database(rows, **fields), "owner"))
        self.assertFalse(rotation_teardown_is_safe(fixture, self.database(rows), "wrong-owner"))
        fixture._owner_verified = False
        self.assertFalse(rotation_teardown_is_safe(fixture, self.database(rows), "owner"))

    def test_teardown_still_requires_the_power_gate_first(self):
        original = power_fixture.power_teardown_is_safe
        power_fixture.power_teardown_is_safe = lambda *args: False
        try:
            self.assertFalse(rotation_teardown_is_safe(self.fixture_with(None),
                self.database([]), "owner"))
        finally:
            power_fixture.power_teardown_is_safe = original


if rotation_acceptance_enabled(os.environ):
    class E2BRotationConsumptionProductionTests(LiveServerTestCase):
        """Two bootstrap Runs and ten successor switches retain two provider identities."""
        serialized_rollback = False
        product = retained_fixture.product_fixture
        load_worker = retained_fixture.E2BRetainedConsumptionProductionTests.load_worker
        stop_runtime = retained_fixture.E2BRetainedConsumptionProductionTests.stop_runtime
        admit = retained_fixture.E2BRetainedConsumptionProductionTests.admit
        claim_due_job = retained_fixture.E2BRetainedConsumptionProductionTests.claim_due_job
        creation_target = retained_fixture.E2BRetainedConsumptionProductionTests.creation_target
        execute_retained = retained_fixture.E2BRetainedConsumptionProductionTests.execute_retained
        assert_provider_absent = retained_fixture.E2BRetainedConsumptionProductionTests.assert_provider_absent

        def setUp(self):
            selected = json.loads(self.product.Path(os.environ['RUNTIME_E2B_PROFILE_PATH']).read_bytes())
            if (selected.get('workspaceInitialization') != 'taskImage' or selected.get('user') != 'root'):
                raise AssertionError('rotation_task_image_root_profile_required')
            self.rotation_runs = {}
            self.targets = {}
            self.visits = {"A": 0, "B": 0}
            self.sessions = {}
            self.power_observations = PowerTimingCollector(max_records=1024, max_operations=64)
            factory = self.product.workspace_observation_collector

            def combined_collector(environment):
                workspace = factory(environment)
                return PowerTimingMultiplexer(workspace, self.power_observations) if workspace is not None else None

            with patch.object(self.product, "workspace_observation_collector", side_effect=combined_collector):
                retained_fixture.E2BRetainedConsumptionProductionTests.setUp(self)
            self.run_segments = []
            self.resource_facts = {}
            self.directed_checks = {}
            self.cancelled_runs = set()
            self.directed_lost_reply_attempt = None
            self._last_power_sequence = 0
            self._binder_stop = threading.Event()
            self._binder = threading.Thread(target=self._bind_power_operations, daemon=True)
            self._binder.start()
            self.addCleanup(self._binder_stop.set)

        def start_runtime(self):
            with patch.dict(os.environ, {"RUNTIME_E2B_MAX_RUNNING_ENVIRONMENTS": "1",
                                         "RUNTIME_E2B_IDLE_PAUSE_MS": "3000"}):
                return retained_fixture.E2BRetainedConsumptionProductionTests.start_runtime(self)

        def _fixture_teardown(self):
            if not rotation_teardown_is_safe(self, connection,
                    os.environ.get("CENTAERIS_E2B_PRODUCT_DB_OWNER")):
                print("e2b-rotation-responsibility:pending", flush=True)
                self.fail("Unresolved rotation responsibility remains; owned database must remain")
            super()._fixture_teardown()

        # ---------- measurement helpers ----------

        def _bind_power_operations(self):
            """Bind each durable power row before its dispatch records stream out."""
            seen = set()
            while not self._binder_stop.is_set():
                try:
                    with connection.cursor() as cursor:
                        cursor.execute("SELECT operation_id,action FROM runtime.provider_environment_power_operations")
                        rows = cursor.fetchall()
                    for operation_id, action in rows:
                        if operation_id not in seen:
                            seen.add(operation_id)
                            self.power_observations.bind(operation_id, action)
                except Exception:
                    pass
                self._binder_stop.wait(0.1)

        def pg_stats(self):
            with connection.cursor() as cursor:
                cursor.execute("SELECT " + ",".join(PG_STAT_COLUMNS)
                    + " FROM pg_stat_database WHERE datname=current_database()")
                row = cursor.fetchone()
            return dict(zip(PG_STAT_COLUMNS, (int(value) for value in row)))

        @staticmethod
        def pg_delta(before, after):
            return {key: after[key] - before[key] for key in PG_STAT_COLUMNS}

        def active_resident_count(self):
            with connection.cursor() as cursor:
                cursor.execute("SELECT count(*) FROM runtime.provider_creation_operations c LEFT JOIN LATERAL "
                    "(SELECT p.action,p.ack_json FROM runtime.provider_environment_power_operations p "
                    "WHERE p.environment_id=c.environment_id AND p.request_json::jsonb->>'creationAttemptId'=c.attempt_id "
                    "AND p.dispatch_state<>'notDispatched' ORDER BY p.operation_sequence DESC LIMIT 1) p ON true "
                    "WHERE c.provider='e2b' AND c.dispatch_state='dispatched' AND c.settlement_id IS NULL "
                    "AND NOT COALESCE(p.action='pause' AND p.ack_json IS NOT NULL,false)")
                return int(cursor.fetchone()[0])

        def power_operations(self, environment_ids, since_sequence=0):
            with connection.cursor() as cursor:
                cursor.execute("SELECT operation_id,environment_id,action,operation_sequence,dispatch_state,"
                    "recorded_at_ms,dispatched_at_ms,acknowledged_at_ms,not_dispatched_at_ms "
                    "FROM runtime.provider_environment_power_operations WHERE environment_id=ANY(%s) "
                    "AND operation_sequence>%s ORDER BY operation_sequence", [list(environment_ids), since_sequence])
                names = ("operationId", "environmentId", "action", "sequence", "dispatchState",
                    "recordedAtMs", "dispatchedAtMs", "acknowledgedAtMs", "notDispatchedAtMs")
                return [dict(zip(names, row)) for row in cursor.fetchall()]

        def wake_events(self, job_id):
            with connection.cursor() as cursor:
                cursor.execute("SELECT event_id,at_ms,payload_json FROM runtime.runtime_events "
                    "WHERE task_id=%s AND event_type='runtime_job_wake_requested' ORDER BY at_ms,event_id", [job_id])
                return [{"eventId": row[0], "atMs": row[1], "payload": json.loads(row[2])} for row in cursor.fetchall()]

        def wait_for(self, predicate, deadline, description, poll=0.1):
            while time.monotonic() < deadline:
                value = predicate()
                if value:
                    return value
                time.sleep(poll)
            self.fail("e2b_rotation_wait_timeout:" + description)

        def observe_resident_facts(self, role):
            target = self.targets[role]
            body = {"schema": "runtime.provider_environment.observe.v1", "sessionId": target["sessionId"],
                "environmentId": target["environmentId"]}
            request = Request(self.runtime_url + "/internal/environments/observe",
                data=json.dumps(body).encode(),
                headers={"Content-Type": "application/json", "X-Internal-Token": settings.INTERNAL_API_TOKEN,
                    "X-Environment-Maintenance-Token": self.maintenance_token}, method="POST")
            try:
                try:
                    response = build_opener(ProxyHandler({})).open(request, timeout=30)
                except HTTPError as error:
                    response = error
                with response:
                    encoded = response.read(65537)
                    self.assertEqual(response.status, 200, encoded[:512].decode("utf-8", "replace"))
                    observed = json.loads(encoded)
            except Exception:
                raise AssertionError("e2b_rotation_observe_unavailable") from None
            self.assertEqual(observed.get("state"), "resident")
            self.assertEqual(observed["providerTarget"]["sandboxId"], target["sandboxId"])
            facts = observed.get("resourceFacts")
            self.assertIsInstance(facts, dict)
            self.assertGreater(facts["cpuCount"], 0)
            self.assertGreater(facts["memoryMiB"], 0)
            baseline = self.resource_facts.setdefault(role, facts)
            self.assertEqual(facts, baseline, "live CPU/memory facts must stay stable for one template")
            return facts

        def model_response(self, model, request):
            self.assertEqual(model.pk, self.model.pk)
            self.requests.append(deepcopy(request))
            run_id = request["agentRunId"]
            ordinal = sum(item["agentRunId"] == run_id for item in self.requests) - 1
            role, visit, root = self.rotation_runs[run_id]
            commands = rotation_commands(root, self.retained_marker + "-" + role, visit)
            self.assertLessEqual(ordinal, len(commands))
            if ordinal == len(commands):
                return {"text": "Rotation proof complete.", "toolCalls": [], "usage": self.product.zero_usage()}
            return self.product.fake_tool_call(f"e2b-{self.runs.index(run_id)}-{ordinal}",
                "bash", {"command": commands[ordinal]})

        def assert_rotation_ledger(self):
            with connection.cursor() as cursor:
                cursor.execute("SELECT count(*),count(DISTINCT verified_sandbox_id) "
                    "FROM runtime.provider_creation_operations")
                self.assertEqual(cursor.fetchone(), (len(self.targets), len(self.targets)))
                cursor.execute("SELECT count(*) FROM runtime.provider_creation_operations c LEFT JOIN LATERAL "
                    "(SELECT p.action,p.ack_json FROM runtime.provider_environment_power_operations p "
                    "WHERE p.environment_id=c.environment_id AND p.request_json::jsonb->>'creationAttemptId'=c.attempt_id "
                    "ORDER BY p.operation_sequence DESC LIMIT 1) p ON true WHERE c.dispatch_state='dispatched' "
                    "AND c.settlement_id IS NULL AND NOT COALESCE(p.action='pause' AND p.ack_json IS NOT NULL,false)")
                self.assertLessEqual(cursor.fetchone()[0], 1, 'durable running admission exceeded its configured budget')
            for run_id in self.runs:
                if run_id in self.cancelled_runs:
                    continue
                job = self.worker.runtime_request("/internal/jobs/agent_run.lifecycle:" + run_id)["job"]
                self.assertEqual((job["status"], job["retryCount"]), ("succeeded", 0))

        def retire(self, run, target):
            run.environment.refresh_from_db()
            body = {"schema": "runtime.provider_environment.retire.v1",
                "operationId": "e2b-rotation-retire-" + uuid4().hex,
                "environmentId": target["environmentId"], "creationAttemptId": target["creationAttemptId"],
                "sandboxId": target["sandboxId"], "providerConfigDigest": target["providerConfigDigest"],
                "expectedControllerEpoch": run.environment.controller_epoch, "acknowledgeDataLoss": True}
            status, result = retained_fixture.retirement_http(self.runtime_url, body,
                settings.INTERNAL_API_TOKEN, self.maintenance_token)
            self.assertEqual(status, 200)
            self.assertEqual(result.get("status"), "removed")
            self.assert_provider_absent(target["sandboxId"])

        def test_real_e2b_two_bootstraps_ten_successor_switches_one_running_budget(self):
            last_runs = {}
            stats_before = self.pg_stats()
            with patch("app_core.model_adapter.fake_model_response", side_effect=self.model_response), \
                    override_settings(RUNTIME_URL=self.runtime_url):
                for index in range(12):
                    role = "AB"[index % 2]
                    run = self.admit(self.sessions.get(role, "new"), "Advance the retained memory and file proof.")
                    self.visits[role] += 1
                    self.rotation_runs[run.pk] = (role, self.visits[role],
                        run.authorization.payload["environmentProfile"]["workspaceRoot"])
                    run_db_before = self.pg_stats()
                    target = self.execute_retained(run, str(index))
                    self.sessions[role] = run.session_id
                    if role in self.targets:
                        require_rotation_target(target, self.targets[role])
                    else:
                        self.targets[role] = dict(target, sessionId=run.session_id)
                    self.assertEqual(len({item["sandboxId"] for item in self.targets.values()}), len(self.targets))
                    last_runs[role] = run
                    self.assert_rotation_ledger()
                    self.observe_resident_facts(role)
                    first_tool = run.events.filter(payload__type="tool_result").order_by("agent_run_sequence").first()
                    self.run_segments.append({
                        "index": index, "role": role, "runId": run.pk,
                        "jobId": "agent_run.lifecycle:" + run.pk,
                        "firstToolAtUnixMs": first_tool.createdAtMs if first_tool is not None else None,
                        "powerOperations": self.power_operations(
                            [value["environmentId"] for value in self.targets.values()],
                            since_sequence=self._last_power_sequence),
                        "wakeEvents": self.wake_events("agent_run.lifecycle:" + run.pk),
                        "dbDelta": self.pg_delta(run_db_before, self.pg_stats()),
                        "timings": dict(self.timings)})
                    self._last_power_sequence = max((operation["sequence"]
                        for operation in self.run_segments[-1]["powerOperations"]), default=self._last_power_sequence)
                    print("e2b-rotation-progress:" + json.dumps({"completedRuns": index + 1, "creationAttempts": len(self.targets)}), flush=True)
                    if index == 5:
                        self.directed_clean_boundary_restart()
                    if index == 7:
                        self.directed_cancel_waiting_run()
                with connection.cursor() as cursor:
                    cursor.execute("SELECT action,count(*) FROM runtime.provider_environment_power_operations "
                        "WHERE ack_json IS NOT NULL GROUP BY action")
                    operations = dict(cursor.fetchall())
                self.assertGreaterEqual(operations.get("pause", 0), 11)
                self.assertEqual(operations.get("resume"), 10)
                for role, target in self.targets.items():
                    self.retire(last_runs[role], target)
                self.assertTrue(rotation_teardown_is_safe(self, connection,
                    os.environ.get("CENTAERIS_E2B_PRODUCT_DB_OWNER")))
            stats_after = self.pg_stats()
            self.lifecycle_passed = True
            self.stop_runtime()
            self.assertTrue(self.retained_stream_completions and all(self.retained_stream_completions),
                f"runtime exits must be clean: {self.retained_stream_completions}")
            power_timings = self.power_observations.summary(lifecycle_passed=True,
                stream_drained=all(self.retained_stream_completions))
            print("e2b-rotation-product-measurements:" + json.dumps({
                "schema": "e2b.rotation.acceptance.v1", "initialRuns": 2,
                "successorAlternations": 10, "totalRuns": 12,
                "environments": 2, "creationAttempts": 2, "runningBudget": 1,
                "memoryAndFileVisits": self.visits, "coreTaskServiceMemoryVerified": True,
                "resourceFacts": self.resource_facts,
                "runSegments": self.run_segments,
                "powerAcknowledgements": operations,
                "databaseTotals": self.pg_delta(stats_before, stats_after),
                "directedChecks": self.directed_checks,
                "rotationProviderAbsent": True, "rotationPendingDuties": 0,
                "databaseCounterScope": "whole owned database, including fixture polling; not product-only query amplification",
                "productOnlyDatabaseAmplification": "unavailable",
                "clockBoundary": CLOCK_BOUNDARY, "metricBoundary": METRIC_BOUNDARY,
                "powerTimings": power_timings,
                "timings": self.timings}, sort_keys=True, allow_nan=False), flush=True)

        # ---------- directed checks ----------

        def directed_clean_boundary_restart(self):
            started = time.monotonic()
            environment_ids = [value["environmentId"] for value in self.targets.values()]
            before = self.power_operations(environment_ids)
            retained_fixture.restart_retained_runtime(self)
            settings.RUNTIME_URL = self.runtime_url
            elapsed = round((time.monotonic() - started) * 1000, 3)
            self.assertEqual(self.power_operations(environment_ids), before,
                "a clean-boundary restart must not rewrite settled power facts")
            for role, target in self.targets.items():
                current = self.creation_target(target["environmentId"])
                self.assertEqual((current["sandboxId"], current["instanceId"]),
                    (target["sandboxId"], target["instanceId"]))
            self.directed_checks["cleanBoundaryRestart"] = {"restartMs": elapsed, "powerFactsUnchanged": True}

        def directed_cancel_waiting_run(self):
            run = self.admit(self.sessions["A"], "This waiting Run is cancelled before any capacity is granted.")
            job_id = "agent_run.lifecycle:" + run.pk

            def parked():
                job = self.worker.runtime_request("/internal/jobs/" + job_id)["job"]
                return job if job["status"] == "queued" and job["runAtMs"] > self.worker.now_ms() else None

            job = self.claim_due_job(self.worker, job_id, "agent_run.lifecycle", "worker:rotation-cancel")
            self.worker.start_job(job_id, job["leaseOwner"])
            self.assertFalse(self.worker.execute_agent_run_lifecycle_job(job, job["leaseOwner"], lambda: None))
            self.wait_for(parked, time.monotonic() + 120, "parked capacity wait")
            with connection.cursor() as cursor:
                cursor.execute("SELECT count(*) FROM runtime.provider_creation_operations")
                creations_before = cursor.fetchone()[0]
            active_before = self.active_resident_count()
            response = self.client.post(f"/api/sessions/{run.session_id}"
                f"/agent-runs/{run.pk}/cancel")
            self.assertIn(response.status_code, (200, 202), response.content)
            self.assertIn(response.json().get("disposition"), ("terminal", "requested"))

            def terminal():
                run.refresh_from_db()
                job = self.worker.runtime_request("/internal/jobs/" + job_id)["job"]
                if run.status == "cancelled" and job["status"] in ("succeeded", "dead", "cancelled"):
                    return run
                for stage in ("advance", "cleanup"):
                    control = self.worker.claim_control_job(stage, "worker:rotation-cancel-control")
                    if control is not None:
                        self.assertEqual(control["jobId"], job_id)
                        self.worker.execute_control_job(control)
                return None

            self.wait_for(terminal, time.monotonic() + 120, "cancelled run terminal")
            with connection.cursor() as cursor:
                cursor.execute("SELECT count(*) FROM runtime.provider_creation_operations")
                self.assertEqual(cursor.fetchone()[0], creations_before,
                    "cancelling a capacity-waiting Run must not create an environment")
            self.assertLessEqual(self.active_resident_count(), active_before)
            self.assertFalse(run.events.filter(payload__type__in=("tool_call", "model_request_started")).exists(),
                "a cancelled waiting Run must not have executed anything")
            self.cancelled_runs.add(run.pk)
            self.directed_checks["cancelWaitingRun"] = {"runId": run.pk,
                "creationCountUnchanged": True, "executedNothing": True}

        def test_real_e2b_lost_sdk_reply_keeps_unknown_and_budget_across_restart(self):
            """A business failure does not settle an uncertain physical creation."""
            with override_settings(RUNTIME_URL=self.runtime_url):
                self.directed_lost_sdk_reply()
            self.lifecycle_passed = True
            print("e2b-lost-reply-measurements:" + json.dumps({
                "schema": "e2b.rotation.lostReplyRecovery.v1", **self.directed_checks["lostSdkReply"],
                "pendingDuties": 1, "databasePreserved": True}), flush=True)

        def directed_lost_sdk_reply(self):
            run = self.admit("new", "This creation loses its SDK reply after dispatch.")
            job_id = "agent_run.lifecycle:" + run.pk
            environment_id = run.environment_id

            def dispatched_creation():
                with connection.cursor() as cursor:
                    cursor.execute("SELECT attempt_id,dispatch_state FROM runtime.provider_creation_operations "
                        "WHERE environment_id=%s", [environment_id])
                    row = cursor.fetchone()
                return row if row is not None and row[1] == "dispatched" else None

            job = self.claim_due_job(self.worker, job_id, "agent_run.lifecycle", "worker:rotation-lost-reply")
            self.worker.start_job(job_id, job["leaseOwner"])
            errors = []
            def execute():
                try:
                    self.worker.execute_agent_run_lifecycle_job(job, job["leaseOwner"], lambda: None)
                except Exception as error:
                    errors.append(error)
            executor = threading.Thread(target=execute, daemon=True)
            executor.start()
            attempt = self.wait_for(dispatched_creation, time.monotonic() + 120, "creation dispatch")[0]
            self.wait_for(lambda: kill_adapter_processes() > 0, time.monotonic() + 30,
                "adapter killed for the lost reply")

            def failed_after_loss():
                run.refresh_from_db()
                return run if run.status == "failed" else None

            self.wait_for(failed_after_loss, time.monotonic() + 60, "business failure after lost reply")
            executor.join(timeout=45)
            self.assertFalse(executor.is_alive(), "lost reply worker must relinquish its slot")
            self.assertEqual(len(errors), 1)
            self.assertIsInstance(errors[0], self.worker.DependencyUnavailable)
            self.assertEqual(errors[0].http_status, 503, "unknown cleanup must remain unconfirmed")
            retained_fixture.restart_retained_runtime(self)
            settings.RUNTIME_URL = self.runtime_url

            def unknown_unchanged():
                with connection.cursor() as cursor:
                    cursor.execute("SELECT dispatch_state,reported_sandbox_id,verified_sandbox_id,settlement_id "
                        "FROM runtime.provider_creation_operations WHERE attempt_id=%s", [attempt])
                    row = cursor.fetchone()
                    cursor.execute("SELECT count(*) FROM runtime.provider_creation_operations WHERE environment_id=%s",
                        [environment_id])
                    return row + (cursor.fetchone()[0],) if row is not None else None

            final = self.wait_for(lambda: preserved_unknown_creation(unknown_unchanged),
                time.monotonic() + 30, "unknown creation stays unfabricated")
            self.assertIsNotNone(final)
            self.assertGreaterEqual(self.active_resident_count(), 1,
                "the Unknown creation must keep holding its running budget")
            run.refresh_from_db()
            self.assertEqual(run.status, "failed")
            self.assertEqual(run.events.filter(payload__type="agent_run_failed").count(), 1)
            self.assertFalse(run.events.filter(payload__type="agent_run_completed").exists())
            self.directed_lost_reply_attempt = attempt
            self.runs.remove(run.pk)
            self.directed_checks["lostSdkReply"] = {"runId": run.pk, "attemptId": attempt,
                "runState": "failed", "unknownKept": True, "budgetHeld": True, "notRedispatchedAfterRestart": True}
