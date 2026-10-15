"""Read-only live observations of the owned long-Run fixture; no task mutation."""
import argparse
import json
import inspect
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



def durable_compaction_facts(records, session_id, cursor, required_summary=None, after_sequence=0):
    """Bind one committed summary without fetching full snapshots or prompt bodies."""
    import hashlib
    import json
    import re
    def fail(message):raise RuntimeError('durable compaction '+message)
    def digest(value):return 'sha256:'+hashlib.sha256(value.encode()).hexdigest()
    commits=[event for event in records if event.payload.get('type')=='compaction']
    if required_summary is not None:
        commits=[event for event in commits if event.payload['payload'].get('summaryMessageId')==required_summary]
        if len(commits)!=1:fail('required summary missing or duplicated')
    if not commits:return None
    event=commits[-1];body=event.payload['payload'];summary_id=body['summaryMessageId']
    summary=body['summaryMarkdown']
    if not summary.strip() or len(summary.encode())>262144:fail('summary body invalid')
    cursor.execute("SELECT item->>'content',item->'metadata',message_ordinal-1 FROM runtime.session_runtime_snapshots s CROSS JOIN LATERAL jsonb_array_elements(s.snapshot_json::jsonb->'messages') WITH ORDINALITY AS messages(item,message_ordinal) WHERE s.session_id=%s AND item->>'message_id'=%s",[session_id,summary_id])
    snapshot_rows=cursor.fetchall()
    if not snapshot_rows and required_summary is None:return None
    if len(snapshot_rows)!=1:fail('persisted summary missing or duplicated')
    content,metadata,position=snapshot_rows[0]
    if isinstance(metadata,str):metadata=json.loads(metadata)
    if (content!=summary or metadata.get('kind')!='context_compaction'
            or metadata.get('compaction_id')!=body['compactionId']
            or metadata.get('first_kept_message_id')!=body.get('firstKeptMessageId')):
        fail('persisted summary identity changed')
    requests=[r for r in records if r.payload.get('type')=='model_request_started'
        and r.payload['payload'].get('purpose')=='compaction'
        and r.agent_run_sequence<event.agent_run_sequence]
    previous=max((r.agent_run_sequence for r in records if r.payload.get('type')=='compaction'
        and r.agent_run_sequence<event.agent_run_sequence),default=0)
    if not requests or requests[-1].agent_run_sequence<=previous:fail('model request binding missing')
    request=requests[-1];payload=request.payload['payload']
    cursor.execute('SELECT manifest_digest,manifest_json FROM runtime.model_observation_manifests WHERE session_id=%s',[session_id])
    rows=cursor.fetchall()
    if len(rows)>15000 or sum(len(raw.encode()) for _,raw in rows)>64*1024*1024:fail('manifest observation bound exceeded')
    manifests={}
    for key,raw in rows:
        if key!=digest('centaeris.model_observation_manifest.v1\0'+raw):fail('manifest digest mismatch')
        manifests[key]=json.loads(raw)
    def references(item):
        reference=item.payload['payload']['observations']
        if set(reference)!={'manifestDigest'}:fail('request manifest invalid')
        root=reference['manifestDigest'];chain=[];seen=set()
        while root is not None:
            if root in seen or root not in manifests:fail('manifest chain incomplete')
            seen.add(root);manifest=manifests[root]
            if set(manifest)!={'parentDigest','observationCount','changes'}:fail('manifest fields invalid')
            chain.append(manifest);root=manifest['parentDigest']
        refs=[]
        for manifest in reversed(chain):
            count=manifest['observationCount'];changed=set()
            if type(count) is not int or count<0:fail('observation count invalid')
            for change in manifest['changes']:
                i=change.get('index')
                if (set(change)!={'index','kind','contentDigest'} or type(i) is not int
                        or not 0<=i<count or i>len(refs) or i in changed):fail('manifest changes invalid')
                changed.add(i)
                if i==len(refs):refs.append(change)
                else:refs[i]=change
            refs=refs[:count]
            if len(refs)!=count:fail('manifest references incomplete')
        return refs
    prompts=[ref for ref in references(request) if ref['kind']=='compaction_prompt']
    if len(prompts)!=1:fail('request has no unique compaction prompt')
    cursor.execute('SELECT kind,content_bytes FROM runtime.model_observation_contents WHERE session_id=%s AND content_digest=%s',[session_id,prompts[0]['contentDigest']])
    stored=cursor.fetchall()
    if len(stored)!=1 or stored[0][0]!='compaction_prompt' or not 0<stored[0][1]<=1048576:fail('compaction prompt reference missing')
    cursor.execute("SELECT content_digest,content_json FROM runtime.model_observation_contents WHERE session_id=%s AND kind='message' AND content_json::jsonb->'message'->>'messageId'=%s",[session_id,summary_id])
    matching=cursor.fetchall()
    summary_digests=set()
    for key,raw in matching:
        if key!=digest('centaeris.model_observation_content.v1\0'+raw):fail('summary observation digest mismatch')
        observation=json.loads(raw);message=observation.get('message',{})
        if (observation.get('kind')!='message' or message.get('role')!='system'
                or message.get('messageId')!=summary_id or message.get('content')!=summary):fail('summary observation changed')
        summary_digests.add(key)
    main=None
    for candidate in reversed(records):
        if (candidate.payload.get('type')!='model_request_started'
                or candidate.payload['payload'].get('purpose')!='main'
                or candidate.agent_run_sequence<=max(after_sequence,event.agent_run_sequence)):
            continue
        matched=[ref for ref in references(candidate) if ref['kind']=='message' and ref['contentDigest'] in summary_digests]
        if matched:
            next_main=min((r.agent_run_sequence for r in records if r.payload.get('type')=='model_request_started'
                and r.payload['payload'].get('purpose')=='main' and r.agent_run_sequence>candidate.agent_run_sequence),default=float('inf'))
            successful=[]
            for call in records:
                value=call.payload.get('payload',{})
                if (call.payload.get('type')!='tool_call' or value.get('toolName')!='bash'
                        or not candidate.agent_run_sequence<call.agent_run_sequence<next_main):continue
                marker=re.search(r'acceptance-long-(\d{4})$',value.get('normalizedInput',{}).get('command',''))
                if marker is None:continue
                results=[r for r in records if r.payload.get('type')=='tool_result'
                    and r.payload.get('payload',{}).get('callId')==value['callId']]
                if not results:continue
                if len(results)!=1:fail('command result duplicated')
                result=results[0];output=result.payload['payload']
                if (not call.agent_run_sequence<result.agent_run_sequence<next_main
                        or output.get('resultState')!='successWithOutput'
                        or 'acceptance-long-'+marker[1] not in output.get('modelContent','')):fail('command result invalid')
                successful.append({'callId':value['callId'],'commandIndex':int(marker[1]),
                    'callSequence':call.agent_run_sequence,'resultSequence':result.agent_run_sequence})
            if not successful:continue
            if len(successful)!=1:fail('main command binding ambiguous')
            main={'requestId':candidate.payload['payload']['requestId'],'sequence':candidate.agent_run_sequence,
                'manifestDigest':candidate.payload['payload']['observations']['manifestDigest'],
                'summaryContentDigest':matched[0]['contentDigest'],'successfulCommand':successful[0]}
            break
    cursor.execute('SELECT count(*) FROM runtime.runtime_events WHERE session_id=%s AND event_type=%s',[session_id,'prompt.compaction.failed'])
    failures=cursor.fetchone()[0]
    return {'compactionId':body['compactionId'],'summaryMessageId':summary_id,
        'summarySha256':digest(summary),'summaryBytes':len(summary.encode()),'snapshotMessagePosition':position,
        'firstKeptMessageId':body.get('firstKeptMessageId'),'sessionEventId':event.eventId,
        'sequence':event.agent_run_sequence,'failureCount':failures,
        'requestBound':{'requestId':payload['requestId'],'sequence':request.agent_run_sequence,
            'manifestDigest':payload['observations']['manifestDigest'],'compactionPromptDigest':prompts[0]['contentDigest']},
        'lastMainObservation':main}


def compaction_recovery_proof(before, current, post_restart_baseline):
    """None means an honest post-restart main request has not been observed yet."""
    old=before['durableCompaction'];new=current.get('durableCompaction')
    if new is None:raise RuntimeError('restarted Run lost its committed compaction summary')
    for key in ('compactionId','summaryMessageId','summarySha256','summaryBytes','sessionEventId','sequence','requestBound'):
        if new.get(key)!=old.get(key):raise RuntimeError('restarted Run replaced its committed compaction '+key)
    if new.get('failureCount')!=0:raise RuntimeError('restarted Run has prompt compaction failures')
    baseline=post_restart_baseline
    if (baseline is None or baseline['highestSessionSequence']<before['highestSessionSequence']
            or baseline['completedCommands']<before['completedCommands']
            or baseline['fence']['controllerId']!=before['fence']['controllerId']
            or baseline['fence']['environmentInstanceId']!=before['fence']['environmentInstanceId']
            or baseline['fence']['controllerEpoch']<=before['fence']['controllerEpoch']
            or current['fence']!=baseline['fence']):
        raise RuntimeError('summary recovery has no stable newer same-Run controller')
    main=new.get('lastMainObservation')
    if main is None or main['sequence']<=baseline['highestSessionSequence']:return None
    command=main.get('successfulCommand')
    if (command is None or command['callSequence']<=main['sequence']
            or command['resultSequence']<=command['callSequence']
            or command['commandIndex']<baseline['completedCommands']
            or current['completedCommands']<=command['commandIndex']):return None
    if current['completedCommands']<=baseline['completedCommands']:return None
    return {'compactionId':new['compactionId'],'summaryMessageId':new['summaryMessageId'],
        'summarySha256':new['summarySha256'],'originalCompactionRequestId':new['requestBound']['requestId'],
        'postRestartMainObservation':main,'postRestartCommandResult':command,
        'postRestartBaseline':baseline,'completedCommandsBefore':before['completedCommands'],
        'completedCommandsAfter':current['completedCommands']}

def main():
    parser=argparse.ArgumentParser()
    parser.add_argument('--compose',required=True,type=Path)
    parser.add_argument('--run',required=True)
    parser.add_argument('--previous',required=True,type=Path)
    parser.add_argument('--result',required=True,type=Path)
    parser.add_argument('--compaction-proof',type=Path)
    args=parser.parse_args()
    previous=json.loads(args.previous.read_text())
    proof=json.loads(args.compaction_proof.read_text()) if args.compaction_proof else None
    code=inspect.getsource(durable_compaction_facts)+'\n'+"""
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
with connection.cursor() as c:
 compaction=durable_compaction_facts(records,run.session_id,c,SUMMARY_ID_LITERAL,AFTER_SEQUENCE_LITERAL)
print(json.dumps({'agentRunId':run.id,'runStatus':run.status,'containerId':rows[0][0],
 'sessionId':run.session_id,'environmentId':env.id,'instanceId':instances[0][0],
 'completedCommands':len(results),'attemptedCommands':len(calls),
 'highestSessionSequence':max((r.agent_run_sequence for r in records),default=0),'durableCompaction':compaction,
 'fence':{'environmentInstanceId':instances[0][0],'controllerId':run.id,
 'controllerEpoch':env.controller_epoch,'controllerPurpose':env.controller_purpose}}))
""".replace('RUN_ID_LITERAL',repr(args.run)).replace('SUMMARY_ID_LITERAL',repr(proof['durableCompaction']['summaryMessageId'] if proof else None)).replace('AFTER_SEQUENCE_LITERAL',repr(proof['highestSessionSequence'] if proof else 0))
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
