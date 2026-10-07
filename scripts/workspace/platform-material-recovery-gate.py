"""Host-side kill/reclaim gate; only the explicitly owned MCP E2E stack."""
import json
import subprocess
import time

API = "centaeris-platform-mcp-e2e-api-1"
WORKER = "centaeris-platform-mcp-e2e-material-worker-1"


def run(*args):
    return subprocess.check_output(["docker", *args], text=True, timeout=60)


for container in (API, WORKER):
    inspected = json.loads(run("inspect", container))[0]
    assert inspected["Config"]["Labels"]["com.docker.compose.project"] == "centaeris-platform-mcp-e2e"

run("pause", WORKER)
try:
    seeded = json.loads(run("exec", API, "python", "/app/scripts/platform-material-processing-gate.py", "--seed-only", "--pdf"))
finally:
    run("unpause", WORKER)
task = seeded["task"]
deadline, old_container = time.monotonic() + 30, None
while time.monotonic() < deadline:
    ids = run("ps", "-q", "--filter", "label=workspace.material.task=" + task).split()
    if ids:
        old_container = ids[0]
        break
    time.sleep(0.1)
assert old_container, "No running real processor observed"
run("kill", "--signal", "KILL", WORKER)
# Crash leaves the durable lease. Advance only this owned fixture's expiry;
# no production clock or other task state is modified.
code = ("from app_core.models import MaterialProcessingTask; from django.utils import timezone; "
        "from datetime import timedelta; "
        f"t=MaterialProcessingTask.objects.get(pk={task!r}); "
        "assert t.status=='running'; assert t.attemptCount==1; "
        "t.leaseExpiresAt=timezone.now()-timedelta(seconds=1); t.save(update_fields=['leaseExpiresAt'])")
run("exec", API, "python", "manage.py", "shell", "-c", code)
run("start", WORKER)
deadline = time.monotonic() + 180
while time.monotonic() < deadline:
    status = run("exec", API, "python", "manage.py", "shell", "-c",
                 f"from app_core.models import MaterialProcessingTask; print(MaterialProcessingTask.objects.get(pk={task!r}).status)").strip().splitlines()[-1]
    if status == "completed":
        break
    assert status != "failed", "Recovery exhausted its attempt budget"
    time.sleep(1)
else:
    raise RuntimeError("Recovery timed out")
verified = json.loads(run("exec", API, "python", "/app/scripts/platform-material-processing-gate.py", "--verify", task))
assert verified["attempts"] == 2, verified
assert verified["pageCount"] == 100, verified
assert old_container not in run("ps", "-aq", "--filter", "label=workspace.material.task=" + task).split()
print(json.dumps({"stage": "kill-reclaim", "task": task, "recovered": True, "oldContainerRemoved": True}))
