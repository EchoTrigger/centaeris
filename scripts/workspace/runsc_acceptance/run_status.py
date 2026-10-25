"""Read only bounded Run status; never load transcript bodies or issue authority."""
import argparse
import json
from pathlib import Path
import subprocess

SCHEMA = "retained.acceptance.run.status.v1"
FIELDS = {"schema", "agentRunId", "sessionId", "environmentId", "runStatus",
          "highestRunSequence", "releaseReceiptCount", "cleanupComplete", "instanceId", "coreTerminalType"}


def read_status(compose: Path, run_id: str) -> dict:
    if not isinstance(run_id, str) or not run_id or len(run_id) > 160:
        raise RuntimeError("invalid exact Run identity")
    code = r"""
import json
from django.conf import settings
from django.db import connection,transaction
from django.db.models import Max
from app_core.models import AgentRun,AgentRunEnvironmentRelease,SessionEvent
assert settings.DATABASES['default']['NAME']=='retained_acceptance'
with transaction.atomic():
 with connection.cursor() as c:
  c.execute('SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY')
  run=AgentRun.objects.select_related('environment').get(id=RUN_LITERAL)
  if run.environment_id is not None and run.environment.session_id!=run.session_id:
   raise RuntimeError('Run environment/session identity mismatch')
  highest=SessionEvent.objects.filter(agent_run=run,session_level=False).aggregate(highest=Max('agent_run_sequence'))['highest'] or 0
  releases=AgentRunEnvironmentRelease.objects.filter(agent_run=run).count()
  terminal=list(SessionEvent.objects.filter(agent_run=run,session_level=False,payload__type__in=['agent_run_completed','agent_run_failed','agent_run_interrupted']).values_list('payload__type',flat=True)[:2])
  if len(terminal)>1:raise RuntimeError('ambiguous Core terminal facts')
  c.execute('SELECT DISTINCT instance_id,environment_id,session_id FROM runtime.execution_instances WHERE agent_run_id=%s LIMIT 2',[run.id])
  allocations=c.fetchall()
  if len(allocations)>1:raise RuntimeError('ambiguous Run instance identities')
  instance=None
  if allocations:
   instance,environment,session=allocations[0]
   if environment!=run.environment_id or session!=run.session_id:raise RuntimeError('Run instance binding mismatch')
  c.execute('SELECT environment_id,instance_id,cleanup_complete FROM runtime.agent_run_cleanup_receipts WHERE agent_run_id=%s LIMIT 2',[run.id])
  receipts=c.fetchall()
  if len(receipts)>1:raise RuntimeError('ambiguous Run cleanup receipts')
  complete=False
  if receipts:
   environment,receipt_instance,complete=receipts[0]
   if environment!=run.environment_id or receipt_instance!=instance:raise RuntimeError('Run cleanup binding mismatch')
  print(json.dumps({'schema':'retained.acceptance.run.status.v1','agentRunId':run.id,'sessionId':run.session_id,'environmentId':run.environment_id,'runStatus':run.status,'highestRunSequence':highest,'releaseReceiptCount':releases,'cleanupComplete':complete,'instanceId':instance,'coreTerminalType':terminal[0] if terminal else None}))
""".replace("RUN_LITERAL", repr(run_id))
    result = subprocess.run(
        ["docker", "compose", "-f", str(compose), "exec", "-T", "api",
         "/app/.venv/bin/python", "manage.py", "shell", "-c", code],
        stdin=subprocess.DEVNULL, capture_output=True, text=True, check=True, timeout=30)
    if len(result.stdout.encode()) > 65536:
        raise RuntimeError("Run status response exceeded bounded output")
    lines = result.stdout.splitlines()
    try:
        value = json.loads(lines[-1])
    except (IndexError, ValueError) as error:
        raise RuntimeError("Run status JSON unavailable") from error
    if not isinstance(value, dict) or set(value) != FIELDS or value.get("schema") != SCHEMA:
        raise RuntimeError("Run status schema invalid")
    if value["agentRunId"] != run_id or not isinstance(value["sessionId"], str) or not value["sessionId"]:
        raise RuntimeError("Run status identity mismatch")
    for key in ("environmentId", "instanceId"):
        if value[key] is not None and (not isinstance(value[key], str) or not value[key]):
            raise RuntimeError("Run status identity invalid")
    if (value["runStatus"] not in {"queued", "running", "completed", "failed", "cancelled"}
            or type(value["highestRunSequence"]) is not int or value["highestRunSequence"] < 0
            or type(value["releaseReceiptCount"]) is not int or value["releaseReceiptCount"] not in (0, 1)
            or type(value["cleanupComplete"]) is not bool
            or value["coreTerminalType"] not in (None, "agent_run_completed", "agent_run_failed", "agent_run_interrupted")):
        raise RuntimeError("Run status facts invalid")
    return value


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--compose", type=Path, required=True)
    parser.add_argument("--run", required=True)
    args = parser.parse_args()
    print(json.dumps(read_status(args.compose, args.run)))


if __name__ == "__main__":
    main()
