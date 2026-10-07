"""Isolated centaeris-perf restart matrix. Faults target only synthetic run identities."""
import argparse
import datetime
from dataclasses import dataclass
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import time
import urllib.error
import urllib.request
import uuid


ROOT = Path(__file__).resolve().parents[2]
CONTROL_SOURCE = Path(__file__).with_name("control.py")
_spec = importlib.util.spec_from_file_location("restart_matrix_control", CONTROL_SOURCE)
control = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(control)
PROJECT = control.PROJECT
API = "http://localhost:18000"
REPRESENTATIVE = (("queued", "worker"), ("running", "runtime"), ("waiting", "worker"))
CANDIDATE_SERVICES = ("api", "worker", "mock-model")


@dataclass(frozen=True)
class MatrixCell:
    state: str
    fault: str
    applicable: bool | None
    reason: str | None = None


MATRIX = tuple(
    MatrixCell(state, fault, (None if state == "waiting" and fault == "sandbox"
                              else fault != "sandbox" or state == "running"),
               ("a queued run has no live sandbox" if state == "queued" else
                "probe after reaching durable waiting; applicable only if the run still owns a sandbox")
               if fault == "sandbox" and state != "running" else None)
    for state in ("queued", "running", "waiting")
    for fault in ("api", "worker", "runtime", "sandbox")
)


def validate_fault_target(project: str, run_id: str, owned_run_ids) -> None:
    if project != "centaeris-perf":
        raise ValueError("restart matrix requires the isolated centaeris-perf project")
    if run_id not in set(owned_run_ids):
        raise ValueError("fault target is not owned by this experiment")


def verify_conservation(before: dict, after: dict) -> str:
    if after.get("status") != "completed":
        raise RuntimeError("synthetic run did not complete successfully")
    before_tools = before.get("committedToolFacts", {})
    after_tools = after.get("committedToolFacts", {})
    if any(after_tools.get(identity) != count for identity, count in before_tools.items()):
        raise RuntimeError("an already committed tool fact was lost or duplicated")
    if after.get("terminalEvents") != 1:
        raise RuntimeError("terminal event count is not exactly one")
    return "completed"


def snapshot_conservation(before: dict, after: dict) -> dict:
    before_events = {event["eventId"]: event for event in before["events"]}
    after_events = {event["eventId"]: event for event in after["events"]}
    if len(after_events) != len(after["events"]):
        raise RuntimeError("duplicate event identity observed")
    if any(after_events.get(identity) != event for identity, event in before_events.items()):
        raise RuntimeError("a committed pre-fault event was lost or changed")
    terminals = [event for event in after["events"]
                 if event.get("payload", {}).get("type") in
                 {"agent_run_completed", "agent_run_failed", "agent_run_interrupted"}]
    if after["run"]["status"] != "completed" or len(terminals) != 1 or \
            terminals[0]["payload"].get("type") != "agent_run_completed":
        raise RuntimeError("parent run terminal conservation failed")
    tool_facts = {}
    for event in after["events"]:
        wire = event.get("payload") or {}
        event_type = wire.get("type")
        semantic = wire.get("payload") if isinstance(wire.get("payload"), dict) else {}
        call_id = semantic.get("callId")
        if event_type in {"tool_call", "tool_result"} and call_id:
            key = f"{event_type}:{call_id}"
            tool_facts[key] = tool_facts.get(key, 0) + 1
    duplicates = {key: count for key, count in tool_facts.items() if count != 1}
    if duplicates:
        raise RuntimeError("duplicate parent tool event fact observed")
    call_ids = {key.split(":", 1)[1] for key in tool_facts if key.startswith("tool_call:")}
    result_ids = {key.split(":", 1)[1] for key in tool_facts if key.startswith("tool_result:")}
    if not call_ids or call_ids != result_ids:
        raise RuntimeError("parent tool call/result facts are absent or unpaired")
    return {"preFaultEventsPreserved": len(before_events), "terminalEvents": 1,
            "toolEventFacts": tool_facts,
            "externalSideEffectExactlyOnceEstablished": False}


def terminal_integrity(before: dict, after: dict) -> dict:
    before_events = {event["eventId"]: event for event in before["events"]}
    after_events = {event["eventId"]: event for event in after["events"]}
    if len(after_events) != len(after["events"]) or any(
            after_events.get(identity) != event for identity, event in before_events.items()):
        raise RuntimeError("terminal run lost, changed, or duplicated a committed event")
    terminals = [event for event in after["events"] if (event.get("payload") or {}).get("type")
                 in {"agent_run_completed", "agent_run_failed", "agent_run_interrupted"}]
    if len(terminals) != 1:
        raise RuntimeError("terminal run does not have exactly one terminal event")
    return {"preFaultEventsPreserved": len(before_events), "terminalEvents": 1,
            "terminalType": terminals[0]["payload"]["type"]}


def boundary_reached(snapshot: dict, state: str, run_id: str) -> bool:
    run = snapshot.get("run")
    if not run or run.get("status") in {"completed", "failed", "cancelled"}:
        return False
    lifecycle = next((j for j in snapshot.get("jobs", [])
                      if j.get("job_id") == f"agent_run.lifecycle:{run_id}"), None)
    if state == "queued":
        return bool(run.get("status") == "queued" and lifecycle and
                    lifecycle.get("status") == "queued")
    if state == "running":
        event_types = {event.get("payload", {}).get("type")
                       for event in snapshot.get("events", [])}
        durable_recovery = any(c.get("kind") == "recovery" and c.get("status") == "committed"
                               for c in snapshot.get("checkpoints", []))
        return bool(run.get("status") == "running" and lifecycle and
                    lifecycle.get("status") == "running" and
                    {"agent_run_execution_started", "model_request_started"} <= event_types and
                    durable_recovery)
    if state == "waiting":
        return bool(run.get("status") == "running" and lifecycle and
                    lifecycle.get("status") == "queued" and
                    any(c.get("kind") == "wait" and c.get("status") == "waiting" and
                        c.get("done_reason") == "runtime_job"
                        for c in snapshot.get("checkpoints", [])))
    raise ValueError(f"unsupported state boundary: {state}")


def utc_now():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def candidate_image(service: str, experiment_id: str) -> str:
    if service not in CANDIDATE_SERVICES:
        raise ValueError("restart matrix candidate service is unsupported")
    if not experiment_id or any(not (c.isalnum() or c in "-_") for c in experiment_id):
        raise ValueError("candidate image requires a unique experiment id")
    return f"centaeris-perf-{service}:restart-matrix-{experiment_id}"


def compose_resource_constraints(host_config: dict) -> dict:
    result = {}
    mappings = (("CpusetCpus", "cpuset"), ("Memory", "mem_limit"),
                ("MemorySwap", "memswap_limit"), ("PidsLimit", "pids_limit"))
    for source, target in mappings:
        value = host_config.get(source)
        if value not in (None, "", 0, -1):
            result[target] = value
    nano_cpus = host_config.get("NanoCpus")
    if nano_cpus not in (None, 0):
        result["cpus"] = str(nano_cpus / 1_000_000_000).rstrip("0").rstrip(".")
    if host_config.get("CpuPeriod") and host_config.get("CpuQuota") and not nano_cpus:
        result["cpu_period"] = host_config["CpuPeriod"]
        result["cpu_quota"] = host_config["CpuQuota"]
    return result


def candidate_overlay(experiment_id: str, resources=None) -> dict:
    resources = resources or {}
    return {"services": {
        service: {"image": candidate_image(service, experiment_id),
                  **resources.get(service, {})}
        for service in CANDIDATE_SERVICES
    }}


def candidate_compose_command(env_file: Path, overlay_file: Path, action: list[str]):
    command, env = control.compose_command(ROOT, env_file, [])
    return [*command, "-f", overlay_file.as_posix(), *action], env


def validate_candidate_config(config: dict, experiment_id: str) -> None:
    control.validate_config(config, ROOT)
    for service in CANDIDATE_SERVICES:
        if config["services"][service].get("image") != candidate_image(service, experiment_id):
            raise ValueError(f"candidate Compose image mismatch for {service}")


def build_candidates(stack, output: Path, experiment_id: str) -> None:
    """Build isolated candidate tags only. Deployment is intentionally a separate operation."""
    overlay = output / "candidate.compose.json"
    overlay.write_text(json.dumps(candidate_overlay(experiment_id), indent=2), encoding="utf-8")
    config_command, env = candidate_compose_command(stack.env_file, overlay,
                                                     ["config", "--format", "json"])
    config = json.loads(control.run(config_command, env=env, cwd=ROOT))
    validate_candidate_config(config, experiment_id)
    for service in CANDIDATE_SERVICES:
        build_command, build_env = candidate_compose_command(stack.env_file, overlay,
                                                              ["build", service])
        control.run(build_command, env=build_env, cwd=ROOT, timeout=3600)


def deploy_candidates(stack, output: Path, experiment_id: str, services) -> None:
    """Explicitly deploy selected candidates while preserving live resource controls."""
    services = tuple(services)
    if not services or any(service not in CANDIDATE_SERVICES for service in services):
        raise ValueError("candidate deployment services are invalid")
    if not stack.quiet():
        raise RuntimeError("candidate deployment requires an idle isolated stack")
    before = {service: stack.container(service) for service in CANDIDATE_SERVICES}
    raw_host = {service: before[service]["HostConfig"] for service in services}
    resources = {service: compose_resource_constraints(raw_host[service]) for service in services}
    overlay = output / "deployment.compose.json"
    overlay.write_text(json.dumps(candidate_overlay(experiment_id, resources), indent=2),
                       encoding="utf-8")
    (output / "deployment-hostconfig-before.json").write_text(
        json.dumps(raw_host, indent=2), encoding="utf-8")
    config_command, env = candidate_compose_command(stack.env_file, overlay,
                                                     ["config", "--format", "json"])
    config = json.loads(control.run(config_command, env=env, cwd=ROOT))
    validate_candidate_config(config, experiment_id)
    for service in services:
        rendered = config["services"][service]
        for key, value in resources[service].items():
            if str(rendered.get(key)) != str(value):
                raise RuntimeError(f"rendered deployment lost {service} resource field {key}")
    for service in (name for name in ("mock-model", "api", "worker") if name in services):
        command, deploy_env = candidate_compose_command(
            stack.env_file, overlay, ["up", "-d", "--no-deps", "--force-recreate", service])
        control.run(command, env=deploy_env, cwd=ROOT, timeout=600)
    after = {service: stack.container(service) for service in CANDIDATE_SERVICES}
    for service in services:
        if after[service]["Config"]["Image"] != candidate_image(service, experiment_id):
            raise RuntimeError(f"{service} did not deploy the selected candidate")
        actual = compose_resource_constraints(after[service]["HostConfig"])
        if actual != resources[service]:
            raise RuntimeError(f"{service} resource controls changed during candidate deployment")
    for service in set(CANDIDATE_SERVICES) - set(services):
        if after[service]["Id"] != before[service]["Id"]:
            raise RuntimeError(f"non-selected service {service} changed identity")
    (output / "deployment-containers-after.json").write_text(json.dumps({
        service: {"id": after[service]["Id"], "imageId": after[service]["Image"],
                  "imageRef": after[service]["Config"]["Image"],
                  "hostConfig": after[service]["HostConfig"]}
        for service in CANDIDATE_SERVICES}, indent=2), encoding="utf-8")


class ApiClient:
    def __init__(self, email, password):
        self.email = email
        self.password = password
        self.cookies = {}
        self.csrf = None

    def call(self, method, path, payload=None, timeout=30):
        data = json.dumps(payload).encode() if payload is not None else None
        request = urllib.request.Request(API + path, data=data, method=method)
        if data:
            request.add_header("content-type", "application/json")
        if self.cookies:
            request.add_header("Cookie", "; ".join(f"{k}={v}" for k, v in self.cookies.items()))
        if self.csrf and method not in {"GET", "HEAD"}:
            request.add_header("X-CSRFToken", self.csrf)
        try:
            response = urllib.request.urlopen(request, timeout=timeout)
        except urllib.error.HTTPError as error:
            response = error
        for header in response.headers.get_all("Set-Cookie") or []:
            pair = header.split(";", 1)[0]
            if "=" in pair:
                name, value = pair.split("=", 1)
                self.cookies[name.strip()] = value.strip()
        body = json.loads(response.read() or b"{}")
        if response.status not in {200, 201, 202}:
            raise RuntimeError(f"API {method} {path} failed with status {response.status}")
        return body

    def login(self):
        self.csrf = self.call("GET", "/api/csrf")["csrfToken"]
        self.call("POST", "/api/login", {"email": self.email, "password": self.password})
        self.csrf = self.call("GET", "/api/csrf")["csrfToken"]


class Experiment:
    def __init__(self, experiment_id, state, fault):
        if not experiment_id or any(not (c.isalnum() or c in "-_") for c in experiment_id):
            raise ValueError("a unique alphanumeric experiment-id is required")
        self.experiment_id = experiment_id
        self.state = state
        self.fault = fault
        self.output = ROOT.parent / "centaeris-perf-evidence" / experiment_id
        self.output.mkdir(parents=True, exist_ok=False)
        self.stack = control.Stack()
        self.owned_run_ids = set()
        self.timeline = self.output / "timeline.jsonl"
        self.client = ApiClient(self.stack.values["BOOTSTRAP_SUPERADMIN_EMAIL"],
                                self.stack.values["BOOTSTRAP_SUPERADMIN_PASSWORD"])

    def record(self, kind, **facts):
        row = {"utc": utc_now(), "databaseEpochMs": int(self.stack.sql(
            "SELECT (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint").strip()),
               "kind": kind, **facts}
        with self.timeline.open("a", encoding="utf-8") as handle:
            handle.write(json.dumps(row, separators=(",", ":")) + "\n")
        return row

    def save_json(self, name, value):
        (self.output / name).write_text(json.dumps(value, indent=2), encoding="utf-8")

    def snapshot(self, run_id, label):
        validate_fault_target(PROJECT, run_id, self.owned_run_ids)
        quoted = run_id.replace("'", "''")
        sql = (
            "SELECT json_build_object("
            "'run',(SELECT row_to_json(r) FROM (SELECT id,session_id,status,\"transitionReason\","
            "\"createdAt\",\"startedAt\",\"completedAt\",\"updatedAt\" FROM app_core_agentrun WHERE id='" + quoted + "') r),"
            "'events',(SELECT coalesce(json_agg(e ORDER BY e.agent_run_sequence),'[]'::json) FROM "
            "(SELECT \"eventId\",agent_run_sequence,payload,\"createdAtMs\",\"insertedAt\" FROM app_core_sessionevent WHERE agent_run_id='" + quoted + "') e),"
            "'jobs',(SELECT coalesce(json_agg(j ORDER BY j.job_id),'[]'::json) FROM "
            "(SELECT job_id,job_kind,status,run_at_ms,lease_owner,lease_expires_at_ms,retry_count,"
            "last_error,updated_at_ms,heartbeat_at_ms,session_id,branch_id,checkpoint_id,payload_ref "
            "FROM runtime.runtime_jobs WHERE job_id='agent_run.lifecycle:" + quoted + "' OR session_id=(SELECT session_id FROM app_core_agentrun WHERE id='" + quoted + "')) j),"
            "'checkpoints',(SELECT coalesce(json_agg(c ORDER BY c.checkpoint_id),'[]'::json) FROM "
            "(SELECT checkpoint_id,kind,session_id,turn_id,status,done_reason,updated_at_ms,payload_json "
            "FROM runtime.checkpoints WHERE session_id=(SELECT session_id FROM app_core_agentrun WHERE id='" + quoted + "')) c))"
        )
        value = json.loads(self.stack.sql(sql))
        self.save_json(f"{label}.json", value)
        self.record("snapshot", label=label, runId=run_id)
        return value

    def containers(self):
        result = {}
        for service in control.SERVICES + ("mock-model",):
            container = self.stack.container(service)
            result[service] = {"id": container["Id"], "image": container["Image"],
                               "startedAt": container["State"]["StartedAt"],
                               "restartCount": container.get("RestartCount")}
        return result

    def setup_model_and_run(self, profile=None, text=None):
        self.client.login()
        provider = self.client.call("POST", "/api/admin/model-providers", {
            "displayName": f"restart-{self.experiment_id}", "api": "openai-responses",
            "apiBase": "https://mock-model:9999/v1", "secret": "sk-perf-dummy",
        })["provider"]
        suffix = profile or {"queued": "instant", "running": "restart30", "waiting": "waiting"}[self.state]
        model_body = self.client.call("POST", "/api/admin/models", {
            "providerId": provider["id"], "modelName": f"perf-{suffix}",
            "displayName": f"Restart {suffix} {self.experiment_id}",
            "contextTokens": 200000, "maxOutputTokens": 8192, "enabled": True,
        })
        model_id = (model_body.get("model") or model_body)["id"]
        workspace = self.client.call("GET", "/api/workspaces")["workspaces"][0]
        agent = self.client.call("GET", f"/api/workspaces/{workspace['id']}/agents")["agents"][0]
        text = text or ("restart-matrix parent waiting" if self.state == "waiting" else
                        f"restart matrix {self.state} {self.fault}: call your tool once, then finish")
        body = self.client.call("POST", f"/api/workspaces/{workspace['id']}/sessions/new/messages", {
            "operationId": uuid.uuid4().hex,
            "agentId": agent["id"], "text": text, "attachmentRefs": [], "modelConfigRef": model_id,
        })
        run_id = body["agentRunId"]
        self.owned_run_ids.add(run_id)
        self.save_json("owned-runs.json", sorted(self.owned_run_ids))
        self.record("runAccepted", runId=run_id, sessionId=body.get("sessionId"))
        return run_id

    def wait_for_boundary(self, run_id, timeout=120, state=None):
        state = state or self.state
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            snap = self.snapshot(run_id, "boundary-latest")
            run = snap["run"]
            if boundary_reached(snap, state, run_id):
                return snap
            if run and run["status"] in {"completed", "failed", "cancelled"}:
                raise RuntimeError(f"run reached {run['status']} before {state} boundary")
            time.sleep(.5)
        raise RuntimeError(f"run did not reach {state} boundary")

    def sandbox(self, run_id):
        validate_fault_target(PROJECT, run_id, self.owned_run_ids)
        ids = control.run(["docker", "ps", "-aq", "--filter",
                           f"label=centaeris.agent_run_id={run_id}"]).split()
        containers = json.loads(control.run(["docker", "inspect", *ids])) if ids else []
        for container in containers:
            labels = container["Config"].get("Labels") or {}
            if labels.get("centaeris.managed") != "true" or labels.get("centaeris.agent_run_id") != run_id:
                raise RuntimeError("sandbox ownership mismatch")
        return containers

    def process_fault(self, service):
        before = self.stack.container(service)
        self.record("faultBegin", fault=service, signal="SIGTERM", containerId=before["Id"])
        waiter = subprocess.Popen(["docker", "wait", before["Id"]], stdout=subprocess.PIPE,
                                  stderr=subprocess.PIPE, text=True)
        self.stack.compose(["restart", "--timeout", "10", service], timeout=120)
        try:
            exit_code = int(waiter.communicate(timeout=5)[0].strip())
        except (subprocess.TimeoutExpired, ValueError):
            waiter.kill()
            waiter.communicate()
            exit_code = None
        self.record("processExited", fault=service, containerId=before["Id"], exitCode=exit_code,
                    exitClass=("graceful" if exit_code in {0, 143} else
                               "forced" if exit_code == 137 else "unknown"))
        deadline = time.monotonic() + 120
        while time.monotonic() < deadline:
            try:
                after = self.stack.container(service)
                if after["State"]["Running"] and after["State"]["StartedAt"] != before["State"]["StartedAt"]:
                    self.record("faultRecovered", fault=service, containerId=after["Id"],
                                startedAt=after["State"]["StartedAt"])
                    return
            except RuntimeError:
                pass
            time.sleep(.5)
        raise RuntimeError(f"{service} did not restart")

    def stop_worker_fixture(self):
        before = self.stack.container("worker")
        facts = {"containerId": before["Id"], "image": before["Image"],
                 "hostConfig": before["HostConfig"]}
        self.record("fixtureStopBegin", service="worker", signal="SIGTERM", **facts)
        self.stack.compose(["stop", "--timeout", "10", "worker"])
        stopped = json.loads(control.run(["docker", "inspect", before["Id"]]))[0]
        if stopped["State"]["Running"] or stopped["Id"] != before["Id"]:
            raise RuntimeError("queued fixture worker did not stop with preserved identity")
        self.record("fixtureStopped", service="worker", exitCode=stopped["State"]["ExitCode"], **facts)
        return facts

    def start_worker_fixture(self, facts):
        # start preserves the stopped container's candidate image and HostConfig; up would recreate it.
        self.stack.compose(["start", "worker"])
        after = self.stack.container("worker")
        if (after["Id"] != facts["containerId"] or after["Image"] != facts["image"] or
                after["HostConfig"] != facts["hostConfig"]):
            raise RuntimeError("queued fixture worker identity or HostConfig changed on release")
        self.record("fixtureReleased", service="worker", containerId=after["Id"],
                    image=after["Image"])

    def sandbox_fault(self, run_id):
        sandboxes = self.sandbox(run_id)
        if len(sandboxes) != 1:
            raise RuntimeError(f"expected exactly one owned sandbox, found {len(sandboxes)}")
        sandbox_id = sandboxes[0]["Id"]
        self.record("faultBegin", fault="sandbox", signal="SIGKILL", containerId=sandbox_id)
        control.run(["docker", "kill", "--signal", "KILL", sandbox_id])
        self.record("faultInjected", fault="sandbox", containerId=sandbox_id)


def matrix_plan():
    return [{"state": cell.state, "fault": cell.fault, "applicable": cell.applicable,
             "reason": cell.reason} for cell in MATRIX]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["plan", "build-candidates", "run"])
    parser.add_argument("--experiment-id")
    parser.add_argument("--state", choices=("queued", "running", "waiting"))
    parser.add_argument("--fault", choices=("api", "worker", "runtime", "sandbox"))
    args = parser.parse_args()
    if args.action == "plan":
        print(json.dumps(matrix_plan(), indent=2))
        return
    if args.action == "build-candidates":
        if not args.experiment_id:
            raise ValueError("build-candidates requires --experiment-id")
        output = ROOT.parent / "centaeris-perf-evidence" / args.experiment_id
        output.mkdir(parents=True, exist_ok=False)
        stack = control.Stack()
        build_candidates(stack, output, args.experiment_id)
        (output / "candidate-images.json").write_text(json.dumps({
            service: candidate_image(service, args.experiment_id)
            for service in CANDIDATE_SERVICES}, indent=2), encoding="utf-8")
        print(f"Candidate evidence: {output}")
        return
    if not args.state or not args.fault:
        raise ValueError("run requires --state and --fault")
    cell = next(row for row in MATRIX if row.state == args.state and row.fault == args.fault)
    if cell.applicable is False:
        raise ValueError(cell.reason)
    experiment = Experiment(args.experiment_id, args.state, args.fault)
    worker_fixture = None
    blocker_ids = []
    try:
        experiment.stack.preflight()
        if not experiment.stack.quiet():
            raise RuntimeError("isolated performance stack must be idle")
        experiment.save_json("manifest-before.json", experiment.stack.manifest())
        experiment.save_json("containers-before.json", experiment.containers())
        genuine_queued_worker = args.state == "queued" and args.fault == "worker"
        if args.state == "queued" and not genuine_queued_worker:
            worker_fixture = experiment.stop_worker_fixture()
        if genuine_queued_worker:
            for index in range(2):
                blocker = experiment.setup_model_and_run(
                    profile="restart30", text=f"restart matrix slot blocker {index}")
                blocker_ids.append(blocker)
            for blocker in blocker_ids:
                experiment.wait_for_boundary(blocker, state="running")
            experiment.record("slotsSaturated", blockerRunIds=blocker_ids)
        run_id = experiment.setup_model_and_run(profile="instant" if genuine_queued_worker else None)
        before = experiment.wait_for_boundary(run_id)
        experiment.save_json("boundary.json", before)
        waiting_sandboxes = experiment.sandbox(run_id) if args.state == "waiting" else []
        experiment.record("boundaryReached", state=args.state, runId=run_id,
                          sandboxCount=len(waiting_sandboxes))
        # The target state must still hold at injection time; a stale boundary is invalid evidence.
        injection = experiment.wait_for_boundary(run_id, timeout=2)
        experiment.save_json("injection-boundary.json", injection)
        if args.fault == "sandbox":
            if args.state == "waiting" and not waiting_sandboxes:
                experiment.save_json("outcome.json", {"status": "not-applicable",
                                     "reason": "no sandbox exists at durable waiting boundary"})
                return
            experiment.sandbox_fault(run_id)
        elif genuine_queued_worker:
            experiment.process_fault("worker")
        else:
            experiment.process_fault(args.fault)
        if args.state == "queued" and not genuine_queued_worker:
            experiment.start_worker_fixture(worker_fixture)
            worker_fixture = None
        deadline = time.monotonic() + 240
        final = None
        while time.monotonic() < deadline:
            final = experiment.snapshot(run_id, "final-latest")
            if final["run"] and final["run"]["status"] in {"completed", "failed", "cancelled"}:
                break
            time.sleep(1)
        if not final or final["run"]["status"] not in {"completed", "failed", "cancelled"}:
            experiment.save_json("outcome.json", {"status": "timeout", "runId": run_id,
                                 "state": args.state, "fault": args.fault})
            raise RuntimeError("run did not reach a terminal state after fault")
        if final["run"]["status"] == "completed":
            result_class = "successful-recovery"
            conservation = snapshot_conservation(injection, final)
        else:
            result_class = "explicit-failure"
            conservation = terminal_integrity(injection, final)
        experiment.save_json("containers-after.json", experiment.containers())
        experiment.save_json("outcome.json", {"status": result_class, "terminalStatus": final["run"]["status"], "runId": run_id,
                             "state": args.state, "fault": args.fault,
                             "scenario": ("preexisting-queued-worker-restart"
                                          if genuine_queued_worker
                                          else "state-boundary-fault-injection"),
                             "blockerRunIds": blocker_ids,
                             "conservation": conservation})
    except Exception as error:
        experiment.save_json("failure.json", {"valid": False, "errorType": type(error).__name__,
                                               "message": str(error), "utc": utc_now()})
        raise
    finally:
        if worker_fixture is not None:
            try:
                experiment.start_worker_fixture(worker_fixture)
                experiment.record("fixtureRestoredAfterFailure", service="worker")
            except Exception as restore_error:
                experiment.save_json("worker-restore-failure.json", {
                    "valid": False, "errorType": type(restore_error).__name__,
                    "message": str(restore_error), "utc": utc_now()})


if __name__ == "__main__":
    main()
