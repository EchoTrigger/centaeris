"""Read-only live observations of the owned long-Run fixture; no task mutation."""
import argparse
import json
from pathlib import Path
import subprocess
import sys
from observe import task_rpc


CONTROL_STORAGE_BUDGET=8*1024*1024
OUTPUT_STORAGE_BUDGET=72*1024*1024


class ObservationNotReady(Exception):
    """Only explicit startup or positively observed same-Run fence advancement."""


def validate_resource_bytes(metrics):
    budgets={'controlDirectoryBytes':CONTROL_STORAGE_BUDGET,
        'outputDirectoryBytes':OUTPUT_STORAGE_BUDGET,'instanceMemoryBytes':268435456,'instancePids':128}
    for key,budget in budgets.items():
        if key in metrics and (type(metrics[key]) is not int or not 0<=metrics[key]<=budget):
            raise RuntimeError('live resource budget exceeded or invalid: '+key)


def validate_live(state, tasks, expected_background):
    if tasks.get('status')!='success' or tasks.get('result',{}).get('kind')!='listed':
        raise RuntimeError('live task-service observation unavailable')
    listed=tasks['result']['tasks']
    active=[task for task in listed if task['state']!='exited']
    background=[task for task in active if task['scope']=={'kind':'environment'}]
    if (len(listed)>64 or len(active)>32
            or sorted(task['taskId'] for task in background)!=expected_background
            or any(task['cleanupComplete'] for task in background)):
        raise RuntimeError('long Run lost its background task or exceeded live task budgets')
    if state.get('runStatus') not in ('pending','running','completed'):
        raise RuntimeError('long Run failed before successful completion')
    return {'hotTaskRecords':len(listed),'activeTasks':len(active),
            'backgroundTaskIds':expected_background}


def probe_reclaimed_controller(container, old, current, background_task):
    import uuid
    if (old['controllerId']!=current['controllerId']
            or old['environmentInstanceId']!=current['environmentInstanceId']
            or current['controllerEpoch']<=old['controllerEpoch']):
        raise RuntimeError('Runtime restart did not reclaim the same Run with a newer fence')
    def rejected(operation,**arguments):
        result=task_rpc(container,old,operation,**arguments)
        if result.get('status')!='error' or result.get('error')!='task_controller_fenced':
            raise RuntimeError('pre-restart controller not rejected: '+operation)
        return result
    # Establish the old fence is obsolete before attempting any mutation.
    results={'list':rejected('list')}
    for operation,arguments in (
        ('start',{'scope':{'kind':'run','runId':old['controllerId']},'program':'/bin/true',
            'args':[],'cwd':'/mnt/data','timeoutMs':1000}),
        ('input',{'taskId':background_task,'data':[120],'close':False}),
        ('terminate',{'taskId':background_task}),
        ('endRun',{'runId':old['controllerId']})):
        results[operation]=rejected(operation,operationId=str(old['controllerEpoch'])+':long-stale-'+uuid.uuid4().hex,**arguments)
    alive=task_rpc(container,current,'query',taskId=background_task,cursor=0,maxBytes=4096)
    if (alive.get('status')!='success' or alive.get('result',{}).get('kind')!='task'
            or alive['result']['task'].get('taskId')!=background_task
            or alive['result']['task'].get('state')!='running'
            or alive['result']['task'].get('cleanupComplete') is not False):
        raise RuntimeError('background task did not survive pre-restart stale mutations')
    return {'beforeFence':old,'afterFence':current,'rejections':results,'backgroundStillRunning':True}


def inspect_store(container, fence):
    raw=subprocess.run(['docker','exec','--user','0:0',container,
        '/opt/centaeris/bin/execution_agent','environment-task-store-inspect'],
        check=True,capture_output=True,text=True).stdout
    metrics=json.loads(raw)
    expected={'schema','instanceId','controllerId','controllerEpoch','hotTasks','activeTasks',
        'indexedTasks','indexedRunTasks','controlDirectoryBytes','outputDirectoryBytes'}
    if (set(metrics)!=expected or metrics['schema']!='environment.task.store.inspection.v1'
            or metrics['instanceId']!=fence['environmentInstanceId']
            or metrics['controllerId']!=fence['controllerId']):
        raise RuntimeError('private store inspection schema or authority identity mismatch')
    counts=expected-{'schema','instanceId','controllerId'}
    if any(type(metrics[key]) is not int or metrics[key]<0 for key in counts):
        raise RuntimeError('private store inspection has invalid counts')
    if (metrics['hotTasks']>64 or metrics['activeTasks']>32
            or metrics['indexedTasks']<metrics['hotTasks']
            or metrics['indexedRunTasks']>metrics['indexedTasks']):
        raise RuntimeError('private store inspection violates live or indexed bounds')
    validate_resource_bytes(metrics)
    if metrics['controllerEpoch']>fence['controllerEpoch']:
        raise ObservationNotReady('same-Run controller advanced between read-only snapshots')
    if metrics['controllerEpoch']!=fence['controllerEpoch']:
        raise RuntimeError('private store inspection controller epoch regressed')
    return metrics


def main():
    parser=argparse.ArgumentParser()
    parser.add_argument('--compose',required=True,type=Path)
    parser.add_argument('--run',required=True)
    parser.add_argument('--previous',required=True,type=Path)
    parser.add_argument('--result',required=True,type=Path)
    args=parser.parse_args()
    previous=json.loads(args.previous.read_text())
    code="""
import json
from django.conf import settings
from django.db import connection
from app_core.models import AgentRun,SessionEvent
assert settings.DATABASES['default']['NAME']=='retained_acceptance'
run=AgentRun.objects.select_related('environment').get(id=RUN_ID_LITERAL)
env=run.environment
if run.status=='queued':
 print(json.dumps({'observationStatus':'notReady','reason':'queued'}))
 raise SystemExit(0)
assert env.controller_run_id==run.id
with connection.cursor() as c:
 c.execute('SELECT container_id FROM runtime.resident_sandboxes WHERE environment_id=%s AND state=%s',[env.id,'resident'])
 rows=c.fetchall()
 assert len(rows)==1
 c.execute('SELECT DISTINCT instance_id FROM runtime.execution_instances WHERE agent_run_id=%s AND environment_id=%s',[run.id,env.id])
 instances=c.fetchall()
 assert len(instances)==1
records=list(SessionEvent.objects.filter(agent_run=run,session_id=run.session_id,
 workspace_id=run.workspace_id,session_level=False).order_by('agent_run_sequence'))
calls=[r.payload['payload'] for r in records if r.payload.get('type')=='tool_call'
 and r.payload.get('payload',{}).get('toolName')=='bash'
 and 'acceptance-long-' in r.payload['payload'].get('normalizedInput',{}).get('command','')]
ids={call['callId'] for call in calls}
results=[r.payload['payload'] for r in records if r.payload.get('type')=='tool_result'
 and r.payload.get('payload',{}).get('callId') in ids
 and r.payload['payload'].get('resultState')=='successWithOutput']
assert len(ids)==len(calls)
assert len({r['callId'] for r in results})==len(results)
print(json.dumps({'agentRunId':run.id,'runStatus':run.status,'containerId':rows[0][0],
 'sessionId':run.session_id,'environmentId':env.id,'instanceId':instances[0][0],
 'completedCommands':len(results),'attemptedCommands':len(calls),
 'fence':{'environmentInstanceId':instances[0][0],'controllerId':run.id,
 'controllerEpoch':env.controller_epoch,'controllerPurpose':env.controller_purpose}}))
""".replace('RUN_ID_LITERAL',repr(args.run))
    raw=subprocess.run(['docker','compose','-f',str(args.compose),'exec','-T','api',
        '/app/.venv/bin/python','manage.py','shell','-c',code],check=True,capture_output=True,text=True).stdout
    state=json.loads(next(line for line in reversed(raw.splitlines()) if line.startswith('{')))
    if state.get('observationStatus')=='notReady' and state.get('reason')=='queued':
        raise ObservationNotReady('Run is queued before allocation')
    for key in ('sessionId','environmentId','instanceId','containerId'):
        if state[key]!=previous[key]:raise RuntimeError('long Run changed retained '+key)
    tasks=task_rpc(state['containerId'],state['fence'])
    if tasks.get('status')=='error' and tasks.get('error')=='task_controller_fenced':
        raise ObservationNotReady('persisted fence advanced before live task query')
    state['live']=validate_live(state,tasks,previous['backgroundTaskIds'])
    state['store']=inspect_store(state['containerId'],state['fence'])
    raw=subprocess.run(['docker','exec','--user','0:0',state['containerId'],'/bin/sh','-c',
        'du -sb /run/centaeris/task-control /run/centaeris/task-output; '
        'cat /sys/fs/cgroup/memory.current /sys/fs/cgroup/pids.current'],
        check=True,capture_output=True,text=True).stdout.splitlines()
    if len(raw)!=4:raise RuntimeError('live resource observation incomplete')
    state['live'].update(controlDirectoryBytes=int(raw[0].split()[0]),
        outputDirectoryBytes=int(raw[1].split()[0]),instanceMemoryBytes=int(raw[2]),instancePids=int(raw[3]))
    validate_resource_bytes(state['live'])
    args.result.write_text(json.dumps(state,indent=2))
    print(json.dumps({'status':'observed','completedCommands':state['completedCommands']}))

def cli():
    try:
        main()
    except ObservationNotReady as error:
        print(json.dumps({'schema':'retained.acceptance.observation.v1','status':'notReady',
            'reason':str(error)}),file=sys.stderr)
        return 75
    except Exception as error:
        print(json.dumps({'schema':'retained.acceptance.observation.v1','status':'fatal',
            'errorType':type(error).__name__,'reason':str(error)}),file=sys.stderr)
        return 1
    return 0


if __name__=='__main__':sys.exit(cli())
