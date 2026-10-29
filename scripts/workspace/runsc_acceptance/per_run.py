"""Independent physical-per-Run cleanup evidence; never issue retirement authority."""
from collections import Counter
import json
from pathlib import Path
import re
import sys
import urllib.parse

discovery_root=str(Path(__file__).resolve().parents[1])
if discovery_root not in sys.path:sys.path.insert(0,discovery_root)
from runsc_rotation_acceptance import FixtureDriver
from runsc_acceptance.execution_mode import fixture_execution_mode


def validate_runs(rows, sessions, iterations):
    if (not isinstance(rows,list) or len(rows)!=sessions*iterations
            or type(sessions) is not int or not 2<=sessions<=64
            or type(iterations) is not int or not 1<=iterations<=20):
        raise ValueError('exact perRun cardinality required')
    runs,instances,environment_sessions=set(),set(),{}
    counts=Counter()
    for row in rows:
        if (not isinstance(row,dict) or any(not isinstance(row.get(key),str) or not row[key]
                for key in ('agentRunId','sessionId','environmentId','instanceId'))
                or row['agentRunId'] in runs or row['instanceId'] in instances
                or row.get('runStatus')!='completed' or row.get('coreTerminalType')!='agent_run_completed'
                or row.get('cleanupComplete') is not True
                or type(row.get('releaseReceiptCount')) is not int or row['releaseReceiptCount']!=1):
            raise ValueError('distinct perRun instance and successful terminal cleanup required')
        if environment_sessions.setdefault(row['environmentId'],row['sessionId'])!=row['sessionId']:
            raise ValueError('logical environment belongs to another Session')
        runs.add(row['agentRunId']);instances.add(row['instanceId']);counts[row['sessionId']]+=1
    if len(counts)!=sessions or any(count!=iterations for count in counts.values()):
        raise ValueError('exact public Session reuse and rolling Run count required')
    return {'completedRuns':len(rows),'freshInstanceRuns':len(instances),'cleanupCompleteRuns':len(rows)}


def validate_cleanup(proofs, rows):
    if not isinstance(proofs,list) or len(proofs)!=len(rows):
        raise ValueError('exact original perRun cleanup proof count required')
    expected={row['agentRunId']:row for row in rows};seen,containers=set(),set()
    for proof in proofs:
        if not isinstance(proof,dict):raise ValueError('positive perRun cleanup proof required')
        run=proof.get('agentRunId');binding=expected.get(run)
        if (binding is None or run in seen
                or any(proof.get(key)!=binding[key] for key in ('sessionId','environmentId','instanceId'))
                or not isinstance(proof.get('containerId'),str) or not proof['containerId']
                or proof['containerId'] in containers
                or any(proof.get(key) is not True for key in ('cleanupComplete','physicalRemoved','containerAbsent','historyPreserved'))
                or type(proof.get('residentHoldingCount')) is not int or proof['residentHoldingCount']!=0
                or type(proof.get('cleanupReceiptAtMs')) is not int or proof['cleanupReceiptAtMs']<0):
            raise ValueError('original physical perRun cleanup, absence, zero holding and public history required')
        seen.add(run);containers.add(proof['containerId'])
    return {'physicallyRemovedRuns':len(seen),'absentContainers':len(containers),'zeroHoldingRuns':len(seen),'publicHistoryRuns':len(seen)}


class SharedFixtureDriver(FixtureDriver):
    def __init__(self,*args,**kwargs):
        super().__init__(*args,**kwargs)
        mode=self.manifest.get('executionMode')
        if mode not in ('perRun','retained') or self.seed.get('executionMode')!=mode:
            raise RuntimeError('fixture and seeded execution mode mismatch')
        fixture_execution_mode(self.manifest,json.loads(self.compose.read_text()))


class PerRunFixtureDriver(SharedFixtureDriver):
    def per_run_inventory_absent(self):
        return not self.command(self.docker+['ps','-aq','--no-trunc','--filter','label=centaeris.managed=true']).split()

    def per_run_cleanup(self,admitted):
        code = r"""import json
from django.conf import settings
from django.db import connection,transaction
from app_core.models import AgentRun
assert settings.DATABASES['default']['NAME']=='retained_acceptance'
with transaction.atomic():
 with connection.cursor() as cursor:
  cursor.execute('SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY')
  run=AgentRun.objects.get(pk=RUN_ID,session_id=SESSION_ID)
  cursor.execute('SELECT DISTINCT instance_id,environment_id,session_id FROM runtime.execution_instances WHERE agent_run_id=%s LIMIT 2',[run.id])
  allocations=cursor.fetchall()
  if len(allocations)!=1:raise RuntimeError('exact original Run allocation required')
  instance,environment,session=allocations[0]
  if environment!=run.environment_id or session!=run.session_id:raise RuntimeError('original Run allocation binding rejected')
  cursor.execute('SELECT environment_id,instance_id,cleanup_complete,physical_removed,removed_container_id,recorded_at_ms FROM runtime.agent_run_cleanup_receipts WHERE agent_run_id=%s LIMIT 2',[run.id])
  receipts=cursor.fetchall()
  if len(receipts)!=1:raise RuntimeError('exact original Run cleanup receipt required')
  receipt_environment,receipt_instance,complete,removed,container,at_ms=receipts[0]
  if receipt_environment!=environment or receipt_instance!=instance or complete is not True or removed is not True or not isinstance(container,str) or not container:
   raise RuntimeError('original Run physical removal receipt required')
  cursor.execute('SELECT count(*) FROM runtime.resident_sandboxes WHERE instance_id=%s OR container_id=%s',[instance,container])
  count=cursor.fetchone()[0]
  print(json.dumps({'agentRunId':run.id,'sessionId':session,'environmentId':environment,'instanceId':instance,'containerId':container,'cleanupComplete':complete,'physicalRemoved':removed,'residentHoldingCount':count,'cleanupReceiptAtMs':at_ms}))
""".replace('RUN_ID',repr(admitted['agentRunId'])).replace('SESSION_ID',repr(admitted['sessionId']))
        proof=self.sql(code)
        if (not isinstance(proof,dict) or any(proof.get(key)!=admitted[key] for key in ('agentRunId','sessionId'))
                or any(not isinstance(proof.get(key),str) or not proof[key] for key in ('environmentId','instanceId'))
                or not isinstance(proof.get('containerId'),str) or not re.fullmatch('[a-f0-9]{64}',proof['containerId'])
                or proof.get('physicalRemoved') is not True or proof.get('cleanupComplete') is not True
                or type(proof.get('residentHoldingCount')) is not int or proof['residentHoldingCount']!=0
                or type(proof.get('cleanupReceiptAtMs')) is not int or proof['cleanupReceiptAtMs']<0):
            raise RuntimeError('positive original physical cleanup proof required before absence observation')
        inventory=self.command(self.docker+['ps','-aq','--no-trunc']).split()
        if any(not re.fullmatch('[a-f0-9]{64}',container) for container in inventory):
            raise RuntimeError('exact physical inventory unavailable')
        proof['containerAbsent']=proof['containerId'] not in inventory
        if not proof['containerAbsent']:raise RuntimeError('original perRun container remains present')
        proof['historyPreserved']=self.public_run_history(admitted,proof['environmentId'])
        return proof

    def public_run_history(self,admitted,environment):
        session,run=(urllib.parse.quote(admitted[key],safe='') for key in ('sessionId','agentRunId'))
        base=f'/api/sessions/{session}/agent-runs/{run}'
        binding=self.http(base+'/environment')
        if (not isinstance(binding,dict) or binding.get('executionMode')!='perRun'
                or binding.get('sessionId')!=admitted['sessionId'] or binding.get('agentRunId')!=admitted['agentRunId']
                or binding.get('environmentId')!=environment):
            raise RuntimeError('public original Run environment history binding required')
        with self.opener.open(self.base+base+'/events',timeout=30) as response:
            data=response.read(4*1024**2+1)
        if len(data)>4*1024**2:raise RuntimeError('bounded public Run history required')
        terminals=[];bash_calls=set();bash_results=set();published=False;seen=set()
        for line in data.decode('utf-8').splitlines():
            if not line.startswith('data:'):continue
            item=json.loads(line[5:].strip())
            if not isinstance(item,dict) or item.get('schema')!='session.stream.item.v1' or item.get('agentRunId')!=admitted['agentRunId']:
                raise RuntimeError('public Run history stream binding rejected')
            if item.get('kind')=='live':continue
            event=item.get('event')
            if (item.get('kind')!='committed' or not isinstance(event,dict)
                    or event.get('sessionId')!=admitted['sessionId'] or event.get('agentRunId')!=admitted['agentRunId']
                    or type(event.get('sequence')) is not int or event['sequence']<=0 or item.get('sourceSequence')!=event['sequence']
                    or event['sequence'] in seen or not isinstance(event.get('eventId'),str) or not event['eventId']):
                raise RuntimeError('public committed Run history binding rejected')
            seen.add(event['sequence']);kind=event.get('type');payload=event.get('payload') or {}
            if kind in ('agent_run_completed','agent_run_failed','agent_run_interrupted'):terminals.append(kind)
            if kind=='tool_call' and payload.get('toolName')=='bash':bash_calls.add(payload.get('callId'))
            if kind=='tool_result' and payload.get('toolName')=='bash' and payload.get('resultState') in ('successWithOutput','successNoOutput'):bash_results.add(payload.get('callId'))
            if kind=='artifact_published':published=True
        if terminals!=['agent_run_completed'] or not (bash_calls & bash_results)-{None} or not published:
            raise RuntimeError('public completed Run tools and publication history required after cleanup')
        return True
